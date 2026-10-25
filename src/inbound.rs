//! 入站协议解析：SOCKS5(CONNECT) 与 HTTP(CONNECT / 绝对URI)，按首字节自动识别。

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::OwnedWriteHalf;

use crate::config::is_valid_host;
use crate::t5::MAX_HEADER;
use crate::util::{find, latin1_to_bytes, latin1_to_string};

/// 入站协议数据不合法。
#[derive(Debug)]
pub struct ClientError(pub String);

/// SOCKS5 协议内错误；replied = True 表示应答已由握手阶段发出。
#[derive(Debug)]
pub struct SocksError {
    pub code: u8,
    pub replied: bool,
}

pub enum SocksFail {
    Protocol(SocksError),
    Client(ClientError),
    Io(io::Error),
}

pub enum HttpFail {
    Client(ClientError),
    Io(io::Error),
}

pub enum InboundRequest {
    /// CONNECT：extra 是请求头之后同包到达的应用数据（TLS ClientHello、gRPC 首帧等），
    /// 必须原样转给上游，丢掉会造成“握手成功但首包无响应”。
    Connect {
        host: String,
        port: u16,
        early: Vec<u8>,
    },
    /// 绝对URI 明文 HTTP：replay 是改写后待重放给上游的完整请求。
    Plain {
        host: String,
        port: u16,
        replay: Vec<u8>,
    },
}

/// 在真实流前面垫一段嗅探出来的字节，供协议识别用。
pub struct PreReader<S> {
    stream: S,
    buf: Vec<u8>,
    pos: usize,
}

impl<S: AsyncRead + Unpin> PreReader<S> {
    pub fn new(stream: S, prefix: Vec<u8>) -> Self {
        Self {
            stream,
            buf: prefix,
            pos: 0,
        }
    }

    fn buffered(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn compact(&mut self) {
        if self.pos > 0 && self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
    }

    /// 精确读 n 字节；流提前结束对应 asyncio.IncompleteReadError。
    async fn read_exact_n(&mut self, n: usize) -> io::Result<Vec<u8>> {
        while self.buffered() < n {
            let want = n - self.buffered();
            let mut tmp = vec![0u8; want];
            let got = self.stream.read(&mut tmp).await?;
            if got == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof"));
            }
            self.buf.extend_from_slice(&tmp[..got]);
        }
        let out = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        self.compact();
        Ok(out)
    }

    fn take_head(&mut self) -> Option<(Vec<u8>, Vec<u8>)> {
        let end = find(&self.buf[self.pos..], b"\r\n\r\n")?;
        let head = self.buf[self.pos..self.pos + end + 4].to_vec();
        let extra = self.buf[self.pos + end + 4..].to_vec();
        self.pos = self.buf.len();
        self.compact();
        Some((head, extra))
    }

    /// 读到 CRLFCRLF 为止，返回 (head, 未消费的多余字节)。
    pub async fn read_head(&mut self) -> Result<(Vec<u8>, Vec<u8>), HttpFail> {
        loop {
            if let Some(r) = self.take_head() {
                return Ok(r);
            }
            if self.buffered() > MAX_HEADER {
                return Err(HttpFail::Client(ClientError(
                    "http header too large".into(),
                )));
            }
            let mut tmp = [0u8; 8192];
            match self.stream.read(&mut tmp).await {
                Ok(0) => {
                    return Err(HttpFail::Client(ClientError(
                        "connection closed inside http header".into(),
                    )))
                }
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(e) => return Err(HttpFail::Io(e)),
            }
        }
    }

    pub fn into_inner(self) -> S {
        self.stream
    }
}

pub fn socks_reply(code: u8) -> [u8; 10] {
    [0x05, code, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
}

fn truncated(_: io::Error) -> SocksFail {
    SocksFail::Client(ClientError("socks5 stream truncated".into()))
}

/// 处理 SOCKS5 握手（仅 CONNECT，无鉴权），返回 (host, port)。
pub async fn socks5_accept<S: AsyncRead + Unpin>(
    pr: &mut PreReader<S>,
    wr: &mut OwnedWriteHalf,
) -> Result<(String, u16), SocksFail> {
    let head = pr.read_exact_n(2).await.map_err(truncated)?;
    if head[0] != 5 {
        return Err(SocksFail::Protocol(SocksError {
            code: 1,
            replied: false,
        }));
    }
    let nmethods = head[1] as usize;
    let methods = pr.read_exact_n(nmethods).await.map_err(truncated)?;
    if !methods.contains(&0x00) {
        // RFC 1928: 没有双方都支持的方法时回 0xFF
        let _ = wr.write_all(&[0x05, 0xff]).await;
        return Err(SocksFail::Protocol(SocksError {
            code: 0xff,
            replied: true,
        }));
    }
    wr.write_all(&[0x05, 0x00]).await.map_err(SocksFail::Io)?;

    let req = pr.read_exact_n(4).await.map_err(truncated)?;
    let (ver, cmd, _rsv, atyp) = (req[0], req[1], req[2], req[3]);
    if ver != 5 {
        return Err(SocksFail::Protocol(SocksError {
            code: 1,
            replied: false,
        }));
    }
    if cmd != 1 {
        // 只支持 CONNECT
        return Err(SocksFail::Protocol(SocksError {
            code: 7,
            replied: false,
        }));
    }
    let host = match atyp {
        1 => {
            let b = pr.read_exact_n(4).await.map_err(truncated)?;
            Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string()
        }
        3 => {
            let n = pr.read_exact_n(1).await.map_err(truncated)?[0] as usize;
            let domain = pr.read_exact_n(n).await.map_err(truncated)?;
            latin1_to_string(&domain)
        }
        4 => {
            let b = pr.read_exact_n(16).await.map_err(truncated)?;
            Ipv6Addr::new(
                ((b[0] as u16) << 8) | b[1] as u16,
                ((b[2] as u16) << 8) | b[3] as u16,
                ((b[4] as u16) << 8) | b[5] as u16,
                ((b[6] as u16) << 8) | b[7] as u16,
                ((b[8] as u16) << 8) | b[9] as u16,
                ((b[10] as u16) << 8) | b[11] as u16,
                ((b[12] as u16) << 8) | b[13] as u16,
                ((b[14] as u16) << 8) | b[15] as u16,
            )
            .to_string()
        }
        _ => {
            return Err(SocksFail::Protocol(SocksError {
                code: 8,
                replied: false,
            }))
        }
    };
    let port_b = pr.read_exact_n(2).await.map_err(truncated)?;
    if atyp == 3 && !is_valid_host(&host) {
        return Err(SocksFail::Protocol(SocksError {
            code: 8,
            replied: false,
        }));
    }
    let port = u16::from_be_bytes([port_b[0], port_b[1]]);
    if port == 0 {
        return Err(SocksFail::Protocol(SocksError {
            code: 8,
            replied: false,
        }));
    }
    Ok((host, port))
}

/// 解析入站 HTTP 请求。
pub async fn http_accept<S: AsyncRead + Unpin>(
    pr: &mut PreReader<S>,
) -> Result<InboundRequest, HttpFail> {
    let (head, extra) = pr.read_head().await?;
    let line_end = find(&head, b"\r\n").unwrap_or(head.len());
    let line = latin1_to_string(&head[..line_end]);
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(HttpFail::Client(ClientError(format!(
            "bad request line: {line:?}"
        ))));
    }

    if parts[0].eq_ignore_ascii_case("connect") {
        let (host, port) = match parse_authority(parts[1], 443) {
            Ok(v) => v,
            Err(e) => return Err(HttpFail::Client(e)),
        };
        return Ok(InboundRequest::Connect {
            host,
            port,
            early: extra,
        });
    }

    // 绝对URI 形式的普通 HTTP 请求（curl http_proxy 等场景）
    let (host, port, path) = match split_absolute_uri(parts[1]) {
        Ok(v) => v,
        Err(e) => return Err(HttpFail::Client(e)),
    };
    // 入站明文 HTTP 只转发首个请求：一个隧道绑定一个目标，复用连接会让后续
    // 指向其他主机的请求被塞进旧隧道。明确要求客户端不要复用。
    let mut raw = &head[line_end + 2..];
    if raw.ends_with(b"\r\n\r\n") {
        raw = &raw[..raw.len() - 2];
    }
    let mut headers: Vec<u8> = Vec::with_capacity(raw.len());
    for hl in raw.split_inclusive(|&b| b == b'\n') {
        let drop_line = starts_with_ci(hl, b"proxy-connection:")
            || starts_with_ci(hl, b"proxy-authorization:")
            || starts_with_ci(hl, b"connection:");
        if !drop_line {
            headers.extend_from_slice(hl);
        }
    }
    let mut replay = Vec::with_capacity(headers.len() + extra.len() + 64);
    replay.extend_from_slice(&latin1_to_bytes(parts[0]));
    replay.push(b' ');
    replay.extend_from_slice(&latin1_to_bytes(&path));
    replay.push(b' ');
    replay.extend_from_slice(&latin1_to_bytes(parts[2]));
    replay.extend_from_slice(b"\r\n");
    replay.extend_from_slice(&headers);
    replay.extend_from_slice(b"Connection: close\r\n\r\n");
    replay.extend_from_slice(&extra);
    Ok(InboundRequest::Plain { host, port, replay })
}

fn starts_with_ci(line: &[u8], name: &[u8]) -> bool {
    line.len() >= name.len() && line[..name.len()].eq_ignore_ascii_case(name)
}

/// 解析 host[:port]，拒绝任何可能污染 CONNECT 请求行的字符。
pub fn parse_authority(auth: &str, default_port: u16) -> Result<(String, u16), ClientError> {
    let auth = auth.trim();
    if let Some(rest) = auth.strip_prefix('[') {
        // [::1]:443
        let (host, after) = match rest.find(']') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        let hexish = host
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'a'..='f' | 'A'..='F' | ':' | '.'));
        if host.is_empty() || !hexish {
            return Err(ClientError(format!("bad ipv6 host: {auth:?}")));
        }
        let port = match after.strip_prefix(':') {
            Some(ps) => ps
                .parse::<u16>()
                .map_err(|_| ClientError(format!("bad port: {auth:?}")))?,
            None => default_port,
        };
        if port == 0 {
            return Err(ClientError(format!("bad port: {auth:?}")));
        }
        Ok((host.to_string(), port))
    } else {
        let (host, port) = match auth.rfind(':') {
            Some(i) => {
                let ps = &auth[i + 1..];
                if ps.is_empty() || !ps.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(ClientError(format!("bad authority: {auth:?}")));
                }
                let port = ps
                    .parse::<u16>()
                    .map_err(|_| ClientError(format!("bad port: {auth:?}")))?;
                (&auth[..i], port)
            }
            None => (auth, default_port),
        };
        if !is_valid_host(host) {
            return Err(ClientError(format!("bad host: {auth:?}")));
        }
        if port == 0 {
            return Err(ClientError(format!("bad port: {auth:?}")));
        }
        Ok((host.to_string(), port))
    }
}

/// 等价于 Python `urlsplit()` 对绝对URI 的处理：scheme 必须是 http，
/// netloc 到 '/'、'?'、'#' 为止，host 小写化，'#' 之后的 fragment 丢弃。
fn split_absolute_uri(target: &str) -> Result<(String, u16, String), ClientError> {
    let unsupported = || {
        ClientError(format!(
            "unsupported target: {target:?} (use CONNECT / https for this proxy)"
        ))
    };
    let b = target.as_bytes();
    if b.len() < 7 || !b[..7].eq_ignore_ascii_case(b"http://") {
        return Err(unsupported());
    }
    let rest = &target[7..];
    let netloc_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let netloc = rest[..netloc_end].to_ascii_lowercase();
    let (host, port) = match parse_authority(&netloc, 80) {
        Ok((h, p)) if !h.is_empty() => (h, p),
        Ok(_) => return Err(unsupported()),
        Err(_) => return Err(ClientError(format!("bad port in {target:?}"))),
    };
    let after = &rest[netloc_end..];
    let pathq = match after.find('#') {
        Some(i) => &after[..i],
        None => after,
    };
    let (p, q) = match pathq.find('?') {
        Some(i) => (&pathq[..i], Some(&pathq[i + 1..])),
        None => (pathq, None),
    };
    let mut path = if p.is_empty() {
        "/".to_string()
    } else {
        p.to_string()
    };
    if let Some(q) = q.filter(|s| !s.is_empty()) {
        path.push('?');
        path.push_str(q);
    }
    Ok((host, port, path))
}
