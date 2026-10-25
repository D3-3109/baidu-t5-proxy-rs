//! 节点表加载：每行 '<port> <ip> [node_port]' 或 '<ip> [node_port]'（自动分配端口）。

use std::collections::HashSet;
use std::fs;

use crate::t5::T5_PORT;

#[derive(Clone, Debug)]
pub struct Node {
    pub ip: String,
    pub port: u16,
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub listen_port: u16,
    pub node: Node,
}

/// 与 Python 版 HOST_RE 等价：^[A-Za-z0-9._\-]+$
pub fn is_valid_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || c == b'-')
}

/// 未提供 --config 时的默认节点表。
fn builtin_nodes() -> Vec<Binding> {
    vec![Binding {
        listen_port: 10001,
        node: Node {
            ip: "180.101.50.208".into(),
            port: T5_PORT,
        },
    }]
}

fn digits_only(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}

pub fn load_nodes(path: Option<&str>, port_base: u32) -> Result<Vec<Binding>, String> {
    let mut nodes: Vec<Binding> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut auto = port_base;

    fn add(
        nodes: &mut Vec<Binding>,
        seen: &mut HashSet<u32>,
        port: u32,
        ip: &str,
        nport: u16,
    ) -> Result<(), String> {
        if seen.contains(&port) {
            return Err(format!("config error: duplicate listen port {port}"));
        }
        if port == 0 || port > 65535 || nport == 0 || !is_valid_host(ip) {
            return Err(format!(
                "config error: bad entry -> port={port} ip={ip:?} node_port={nport}"
            ));
        }
        seen.insert(port);
        nodes.push(Binding {
            listen_port: port as u16,
            node: Node {
                ip: ip.to_string(),
                port: nport,
            },
        });
        Ok(())
    }

    let parse_nport = |s: Option<&&str>| -> Result<u16, String> {
        match s {
            None => Ok(T5_PORT),
            Some(text) => {
                if digits_only(text) {
                    text.parse::<u16>()
                        .map_err(|_| format!("config error: bad node_port {text:?}"))
                } else {
                    Err(format!("config error: bad node_port {text:?}"))
                }
            }
        }
    };

    match path {
        Some(p) => {
            let text =
                fs::read_to_string(p).map_err(|e| format!("config error: cannot read {p}: {e}"))?;
            for raw in text.lines() {
                let line = raw.split('#').next().unwrap_or("").trim();
                if line.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if digits_only(parts[0]) && parts.len() >= 2 {
                    let port: u32 = parts[0]
                        .parse()
                        .map_err(|_| format!("config error: bad entry -> port={}", parts[0]))?;
                    let nport = parse_nport(parts.get(2))?;
                    add(&mut nodes, &mut seen, port, parts[1], nport)?;
                } else {
                    let port = auto;
                    auto += 1;
                    let nport = parse_nport(parts.get(1))?;
                    add(&mut nodes, &mut seen, port, parts[0], nport)?;
                }
            }
        }
        None => {
            for b in builtin_nodes() {
                add(
                    &mut nodes,
                    &mut seen,
                    b.listen_port as u32,
                    &b.node.ip,
                    b.node.port,
                )?;
            }
        }
    }
    Ok(nodes)
}
