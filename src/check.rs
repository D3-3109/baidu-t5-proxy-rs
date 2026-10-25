//! --check：并发体检所有节点，经节点访问探测站点并显示出口 IP。

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::config::{Binding, Node};
use crate::t5::{t5_connect, HANDSHAKE_TIMEOUT, MAX_HEADER};
use crate::util::{find, find_ipv4, latin1_to_string};

pub async fn run_checks(nodes: &[Binding], probe_host: &str, probe_path: &str) {
    let mut set = JoinSet::new();
    for b in nodes {
        let node = b.node.clone();
        let ph = probe_host.to_string();
        let pp = probe_path.to_string();
        set.spawn(async move { check_node(&node, &ph, &pp).await });
    }
    while set.join_next().await.is_some() {}
}

async fn check_node(node: &Node, probe_host: &str, probe_path: &str) {
    let ip = &node.ip;
    let nport = node.port;
    match probe(node, probe_host, probe_path).await {
        Ok((status, egress)) => {
            println!("{ip}:{nport:<5} OK    egress={egress:<16} {status}");
        }
        Err(e) => println!("{ip}:{nport:<5} FAIL  {e}"),
    }
}

async fn probe(
    node: &Node,
    probe_host: &str,
    probe_path: &str,
) -> Result<(String, String), String> {
    let (mut stream, leftover) = t5_connect(node, probe_host, 80).await.map_err(|e| e.0)?;
    let req = format!(
        "GET {probe_path} HTTP/1.1\r\nHost: {probe_host}\r\nUser-Agent: t5-check\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| e.to_string())?;

    let mut body = leftover;
    let mut tmp = [0u8; 8192];
    while body.len() < MAX_HEADER {
        match timeout(HANDSHAKE_TIMEOUT, stream.read(&mut tmp)).await {
            Err(_) => return Err("timeout".into()),
            Ok(Err(e)) => return Err(e.to_string()),
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => body.extend_from_slice(&tmp[..n]),
        }
    }

    let (head, rest) = match find(&body, b"\r\n\r\n") {
        Some(i) => (&body[..i], &body[i + 4..]),
        None => (&body[..], &body[body.len()..]),
    };
    let line_end = find(head, b"\r\n").unwrap_or(head.len());
    let status = latin1_to_string(&head[..line_end]);
    let egress = find_ipv4(rest).unwrap_or_else(|| "ip-not-found".into());
    Ok((status, egress))
}
