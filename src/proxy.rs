//! 单连接处理：协议识别 -> 上游握手 -> 双向搬运。

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::config::Node;
use crate::inbound::{http_accept, socks5_accept, HttpFail, InboundRequest, PreReader, SocksFail};
use crate::t5::{t5_connect, HANDSHAKE_TIMEOUT};
use crate::{log_debug, log_info, log_warn};

/// 上游失败时给客户端一个协议内的失败应答。
async fn reject(wr: &mut OwnedWriteHalf, kind: &str) {
    let payload: Vec<u8> = if kind == "socks5" {
        crate::inbound::socks_reply(1).to_vec()
    } else {
        b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n".to_vec()
    };
    let _ = wr.write_all(&payload).await;
}

/// 单向搬运，EOF 时对端做半关闭（shutdown write），返回传输字节数。
async fn pipe<R, W>(mut src: R, mut dst: W, idle_timeout: Option<Duration>) -> u64
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut total = 0u64;
    let mut buf = vec![0u8; 65536];
    loop {
        let res = match idle_timeout {
            Some(t) => match timeout(t, src.read(&mut buf)).await {
                Ok(r) => r,
                Err(_) => break,
            },
            None => src.read(&mut buf).await,
        };
        match res {
            Ok(0) => break,
            Ok(n) => {
                if dst.write_all(&buf[..n]).await.is_err() {
                    break;
                }
                total += n as u64;
            }
            Err(_) => break,
        }
    }
    let _ = dst.shutdown().await;
    total
}

#[allow(clippy::too_many_arguments)]
async fn tunnel(
    peer: &str,
    kind: &str,
    host: String,
    port: u16,
    node: &Arc<Node>,
    idle_timeout: Option<Duration>,
    pr: PreReader<OwnedReadHalf>,
    cli_wr: OwnedWriteHalf,
    up_rd: OwnedReadHalf,
    up_wr: OwnedWriteHalf,
) {
    let cli_rd = pr.into_inner();
    log_info!(
        "[{peer}] {kind} -> {host}:{port} via {}:{}",
        node.ip,
        node.port
    );
    let (sent, recv) = tokio::join!(
        pipe(cli_rd, up_wr, idle_timeout),
        pipe(up_rd, cli_wr, idle_timeout),
    );
    log_debug!("[{peer}] {host}:{port} closed ({sent} out / {recv} in)");
}

pub async fn handle_client(stream: TcpStream, node: Arc<Node>, idle_timeout: Option<Duration>) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "unknown".into());
    let (mut rd, mut wr) = stream.into_split();

    // 按首字节自动识别：0x05 = SOCKS5，其余按 HTTP 解析。
    let mut first = vec![0u8; 8192];
    let n = match timeout(HANDSHAKE_TIMEOUT, rd.read(&mut first)).await {
        Err(_) => {
            log_debug!("[{peer}] inbound idle, closed");
            return;
        }
        Ok(Err(_)) => return,
        Ok(Ok(0)) => return,
        Ok(Ok(n)) => n,
    };
    first.truncate(n);
    let is_socks = first[0] == 5;

    let kind = if is_socks { "socks5" } else { "http" };
    let mut pr = PreReader::new(rd, first);

    if is_socks {
        match socks5_accept(&mut pr, &mut wr).await {
            Err(SocksFail::Protocol(e)) => {
                if !e.replied {
                    let _ = wr.write_all(&crate::inbound::socks_reply(e.code)).await;
                }
            }
            Err(SocksFail::Client(e)) => {
                log_warn!("[{peer}] {kind} bad inbound request: {}", e.0);
                reject(&mut wr, kind).await;
            }
            Err(SocksFail::Io(_)) => {}
            Ok((host, port)) => match t5_connect(&node, &host, port).await {
                Err(e) => {
                    log_warn!("[{peer}] {kind} -> {host}:{port} upstream: {}", e.0);
                    reject(&mut wr, kind).await;
                }
                Ok((up, leftover)) => {
                    let (up_rd, up_wr) = up.into_split();
                    if wr.write_all(&crate::inbound::socks_reply(0)).await.is_err() {
                        return;
                    }
                    if !leftover.is_empty() && wr.write_all(&leftover).await.is_err() {
                        return;
                    }
                    tunnel(
                        &peer,
                        kind,
                        host,
                        port,
                        &node,
                        idle_timeout,
                        pr,
                        wr,
                        up_rd,
                        up_wr,
                    )
                    .await;
                }
            },
        }
    } else {
        match http_accept(&mut pr).await {
            Err(HttpFail::Client(e)) => {
                log_warn!("[{peer}] {kind} bad inbound request: {}", e.0);
                reject(&mut wr, kind).await;
            }
            Err(HttpFail::Io(_)) => {}
            Ok(InboundRequest::Connect { host, port, early }) => {
                match t5_connect(&node, &host, port).await {
                    Err(e) => {
                        log_warn!("[{peer}] {kind} -> {host}:{port} upstream: {}", e.0);
                        reject(&mut wr, kind).await;
                    }
                    Ok((up, leftover)) => {
                        let (up_rd, mut up_wr) = up.into_split();
                        if wr
                            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                            .await
                            .is_err()
                        {
                            return;
                        }
                        if !leftover.is_empty() && wr.write_all(&leftover).await.is_err() {
                            return;
                        }
                        if !early.is_empty() && up_wr.write_all(&early).await.is_err() {
                            return;
                        }
                        tunnel(
                            &peer,
                            kind,
                            host,
                            port,
                            &node,
                            idle_timeout,
                            pr,
                            wr,
                            up_rd,
                            up_wr,
                        )
                        .await;
                    }
                }
            }
            Ok(InboundRequest::Plain { host, port, replay }) => {
                match t5_connect(&node, &host, port).await {
                    Err(e) => {
                        log_warn!("[{peer}] {kind} -> {host}:{port} upstream: {}", e.0);
                        reject(&mut wr, kind).await;
                    }
                    Ok((up, leftover)) => {
                        let (up_rd, mut up_wr) = up.into_split();
                        if up_wr.write_all(&replay).await.is_err() {
                            return;
                        }
                        if !leftover.is_empty() && wr.write_all(&leftover).await.is_err() {
                            return;
                        }
                        tunnel(
                            &peer,
                            kind,
                            host,
                            port,
                            &node,
                            idle_timeout,
                            pr,
                            wr,
                            up_rd,
                            up_wr,
                        )
                        .await;
                    }
                }
            }
        }
    }
}
