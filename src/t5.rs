//! 上游 T5 节点：明文 HTTP CONNECT 握手（逆向自百度系客户端的 T5 加速通道）。
//!
//! ```text
//! CONNECT <真实目标>:<端口> HTTP/1.1\r\n
//! Host: ascdn.baidu.com\r\n
//! Proxy-Connection: Keep-Alive\r\n
//! X-T5-Auth: <token>\r\n
//! User-Agent: okhttp/3.11.0 ... baiduboxapp/...\r\n\r\n
//! ```
//!
//! 收到 2xx 应答后，这条 TCP 连接即成为到目标的透明隧道。

use std::env;
use std::fmt;
use std::io;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::config::Node;
use crate::util;

pub const T5_PORT: u16 = 443;
/// 连接节点 + 等待 CONNECT 应答（秒）。
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// 与 Python 版 asyncio 默认流上限一致（64 KiB）。
pub const UPSTREAM_REPLY_LIMIT: usize = 64 * 1024;
/// 入站头 / 体检响应体的上限。
pub const MAX_HEADER: usize = 256 * 1024;

pub const DEFAULT_AUTH: &str = "1951164069";
pub const DEFAULT_HOST: &str = "ascdn.baidu.com";
pub const DEFAULT_UA: &str = "okhttp/3.11.0 SP-engine/2.71.0 Dalvik/2.1.0 \
(Linux; U; Android 9; HMA-AL00 Build/PQ3B.190801.002) \
baiduboxapp/13.33.0.11 (Baidu; P1 9)";

/// token/UA 可能随百度客户端版本变化，可用环境变量 T5_AUTH / T5_UA / T5_HOST 覆盖。
#[derive(Clone, Debug)]
pub struct T5Params {
    pub auth: String,
    pub host: String,
    pub ua: String,
}

static PARAMS: OnceLock<T5Params> = OnceLock::new();

pub fn params() -> &'static T5Params {
    PARAMS.get_or_init(|| T5Params {
        auth: env::var("T5_AUTH").unwrap_or_else(|_| DEFAULT_AUTH.into()),
        host: env::var("T5_HOST").unwrap_or_else(|_| DEFAULT_HOST.into()),
        ua: env::var("T5_UA").unwrap_or_else(|_| DEFAULT_UA.into()),
    })
}

/// 节点不可达或拒绝 T5 握手。
#[derive(Debug)]
pub struct UpstreamError(pub String);

impl fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

enum HandshakeFail {
    Closed,
    Oversized,
    Io(io::Error),
}

/// 连到节点并完成 T5 CONNECT 握手。
/// 返回 (隧道流, 应答头之后同包到达的隧道数据)；出错时连接已关闭。
pub async fn t5_connect(
    node: &Node,
    host: &str,
    port: u16,
) -> Result<(TcpStream, Vec<u8>), UpstreamError> {
    let ip = &node.ip;
    let nport = node.port;
    let mut stream =
        match timeout(HANDSHAKE_TIMEOUT, TcpStream::connect((ip.as_str(), nport))).await {
            Err(_) => return Err(UpstreamError(format!("{ip}:{nport} connect timeout"))),
            Ok(Err(e)) => return Err(UpstreamError(format!("{ip}:{nport} connect: {e}"))),
            Ok(Ok(s)) => s,
        };

    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let p = params();
    let req = format!(
        "CONNECT {authority} HTTP/1.1\r\n\
         Host: {}\r\n\
         Proxy-Connection: Keep-Alive\r\n\
         X-T5-Auth: {}\r\n\
         User-Agent: {}\r\n\r\n",
        p.host, p.auth, p.ua
    );

    let handshake = async {
        stream
            .write_all(req.as_bytes())
            .await
            .map_err(HandshakeFail::Io)?;
        let mut buf: Vec<u8> = Vec::with_capacity(512);
        let mut tmp = [0u8; 4096];
        loop {
            if let Some(pos) = util::find(&buf, b"\r\n\r\n") {
                let leftover = buf[pos + 4..].to_vec();
                buf.truncate(pos + 4);
                return Ok::<_, HandshakeFail>((buf, leftover));
            }
            if buf.len() > UPSTREAM_REPLY_LIMIT {
                return Err(HandshakeFail::Oversized);
            }
            match stream.read(&mut tmp).await {
                Ok(0) => return Err(HandshakeFail::Closed),
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(e) => return Err(HandshakeFail::Io(e)),
            }
        }
    };

    let (head, leftover) = match timeout(HANDSHAKE_TIMEOUT, handshake).await {
        Err(_) => return Err(UpstreamError(format!("{ip}:{nport} handshake timeout"))),
        Ok(Err(HandshakeFail::Closed)) => {
            return Err(UpstreamError(format!(
                "{ip}:{nport} closed during handshake"
            )))
        }
        Ok(Err(HandshakeFail::Oversized)) => {
            return Err(UpstreamError(format!(
                "{ip}:{nport} oversized handshake reply"
            )))
        }
        Ok(Err(HandshakeFail::Io(e))) => {
            return Err(UpstreamError(format!("{ip}:{nport} handshake: {e}")))
        }
        Ok(Ok(v)) => v,
    };

    let first_line = head.split(|&b| b == b'\n').next().unwrap_or(&[]);
    let first_line = first_line.strip_suffix(b"\r").unwrap_or(first_line);
    let status = util::latin1_to_string(first_line);
    match util::status_code(&status) {
        Some(code) if (200..300).contains(&code) => Ok((stream, leftover)),
        _ => Err(UpstreamError(format!("{ip}:{nport} replied {status:?}"))),
    }
}
