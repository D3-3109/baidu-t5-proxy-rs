//! fake-baidu — 本地自测用的假 T5 节点（fake_baidu.py 的 Rust 版）。
//!
//! 行为和真实节点一致：收到 CONNECT 就应答 200 并把流量直连到目标，
//! 让你在不消耗/不依赖真实节点的情况下验证 baidu-t5-proxy 全链路。
//!
//!     fake-baidu [listen] [port]        # 默认 127.0.0.1 44444
//!
//! 然后在 nodes.txt 里写:  10002 127.0.0.1 44444

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use baidu_t5_proxy::util::find;

const UPSTREAM_REPLY_LIMIT: usize = 64 * 1024;

async fn pipe<R, W>(mut src: R, mut dst: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 65536];
    loop {
        match src.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if dst.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = dst.shutdown().await;
}

async fn handle(stream: TcpStream) {
    let _ = handle_inner(stream).await;
}

async fn handle_inner(mut stream: TcpStream) -> std::io::Result<()> {
    // 读 CONNECT 请求头（应答头之后的字节属于隧道数据，需要带给上游）
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut tmp = [0u8; 4096];
    let (head, leftover) = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            let head = buf[..pos + 4].to_vec();
            let leftover = buf[pos + 4..].to_vec();
            break (head, leftover);
        }
        if buf.len() > UPSTREAM_REPLY_LIMIT {
            return Ok(());
        }
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let line_end = find(&head, b"\r\n").unwrap_or(head.len());
    let line = String::from_utf8_lossy(&head[..line_end]).into_owned();
    let parts: Vec<&str> = line.split_whitespace().collect();
    if !(parts.len() >= 2 && parts[0].eq_ignore_ascii_case("connect")) {
        stream
            .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    }

    let authority = parts[1];
    let (host, port_s) = match authority.rfind(':') {
        Some(i) => (&authority[..i], &authority[i + 1..]),
        None => ("", authority),
    };
    let port: u16 = match port_s.parse() {
        Ok(p) if !host.is_empty() && p > 0 => p,
        _ => {
            let _ = stream
                .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
                .await;
            return Ok(());
        }
    };

    let mut up = TcpStream::connect((host, port)).await?;
    if !leftover.is_empty() {
        up.write_all(&leftover).await?;
    }
    stream
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await?;
    println!("tunnel {authority}");

    let (up_rd, up_wr) = up.into_split();
    let (cli_rd, cli_wr) = stream.into_split();
    let _ = tokio::join!(pipe(cli_rd, up_wr), pipe(up_rd, cli_wr));
    Ok(())
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(44444);

    let listener = TcpListener::bind((host.as_str(), port))
        .await
        .unwrap_or_else(|e| panic!("cannot listen on {host}:{port}: {e}"));
    println!("fake t5 node on {host}:{port}");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        tokio::spawn(handle(stream));
    }
}
