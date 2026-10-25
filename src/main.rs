//! baidu_t5_proxy 的 Rust 版入口：参数解析、节点表加载、监听与 Ctrl-C 处理。

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::signal;

use baidu_t5_proxy::config::{self, Binding};
use baidu_t5_proxy::{check, log_debug, log_info, proxy};

const USAGE: &str = "\
usage: baidu-t5-proxy [-h] [--listen LISTEN] [--config CONFIG] [--port-base PORT_BASE]
                      [--idle-timeout IDLE_TIMEOUT] [--check] [--probe-host PROBE_HOST]
                      [--probe-path PROBE_PATH] [--log-level LOG_LEVEL]

Port-per-node egress proxy for Baidu T5 relays

options:
  -h, --help            show this help message and exit
  --listen LISTEN       listen address (default: 127.0.0.1)
  --config CONFIG       node list file; one '<port> <ip> [node_port]' or
                        '<ip> [node_port]' per line, '#' comments
  --port-base PORT_BASE
                        first port for lines without explicit port (default: 10001)
  --idle-timeout IDLE_TIMEOUT
                        tunnel idle timeout in seconds, 0 disables (default: 900)
  --check               test every node then exit
  --probe-host PROBE_HOST
                        target used by --check (default: myip.ipip.net)
  --probe-path PROBE_PATH
  --log-level LOG_LEVEL
";

struct Args {
    listen: String,
    config: Option<String>,
    port_base: u32,
    idle_timeout: f64,
    check: bool,
    probe_host: String,
    probe_path: String,
    log_level: String,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            listen: "127.0.0.1".into(),
            config: None,
            port_base: 10001,
            idle_timeout: 900.0,
            check: false,
            probe_host: "myip.ipip.net".into(),
            probe_path: "/".into(),
            log_level: "INFO".into(),
        }
    }
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    let mut a = Args::default();
    let mut i = 0usize;
    while i < argv.len() {
        let raw = argv[i].clone();
        if raw == "-h" || raw == "--help" {
            print!("{USAGE}");
            std::process::exit(0);
        }
        let (name, inline) = if raw.starts_with("--") {
            match raw.find('=') {
                Some(p) => (raw[..p].to_string(), Some(raw[p + 1..].to_string())),
                None => (raw.clone(), None),
            }
        } else {
            (raw.clone(), None)
        };
        match name.as_str() {
            "--listen" | "--config" | "--port-base" | "--idle-timeout" | "--probe-host"
            | "--probe-path" | "--log-level" => {
                let val = match inline {
                    Some(x) => {
                        i += 1;
                        x
                    }
                    None => match argv.get(i + 1) {
                        Some(x) => {
                            i += 2;
                            x.clone()
                        }
                        None => {
                            return Err(format!("error: argument {name}: expected one argument"))
                        }
                    },
                };
                match name.as_str() {
                    "--listen" => a.listen = val,
                    "--config" => a.config = Some(val),
                    "--port-base" => {
                        a.port_base = val.parse().map_err(|_| {
                            format!("error: argument --port-base: invalid int value: '{val}'")
                        })?;
                    }
                    "--idle-timeout" => {
                        a.idle_timeout = val.parse().map_err(|_| {
                            format!("error: argument --idle-timeout: invalid float value: '{val}'")
                        })?;
                    }
                    "--probe-host" => a.probe_host = val,
                    "--probe-path" => a.probe_path = val,
                    "--log-level" => a.log_level = val,
                    _ => unreachable!(),
                }
            }
            "--check" => {
                a.check = true;
                i += 1;
            }
            _ => return Err(format!("error: unrecognized arguments: {raw}")),
        }
    }
    Ok(a)
}

async fn serve(args: &Args, nodes: Vec<Binding>, idle: Option<Duration>) {
    let mut listeners = Vec::new();
    for b in &nodes {
        let addr = format!("{}:{}", args.listen, b.listen_port);
        match TcpListener::bind(&addr).await {
            Ok(l) => listeners.push((l, b.clone())),
            Err(e) => {
                eprintln!("error: cannot listen on {addr}: {e}");
                std::process::exit(1);
            }
        }
        log_info!(
            "listening on {}:{} -> node {}:{}",
            args.listen,
            b.listen_port,
            b.node.ip,
            b.node.port
        );
    }
    println!(
        "{} port(s) up on {}, Ctrl-C to stop.",
        listeners.len(),
        args.listen
    );

    for (listener, binding) in listeners {
        let node = Arc::new(binding.node);
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let node = node.clone();
                        tokio::spawn(proxy::handle_client(stream, node, idle));
                    }
                    Err(e) => log_debug!("accept error: {e}"),
                }
            }
        });
    }

    let _ = signal::ctrl_c().await;
    println!("bye.");
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    baidu_t5_proxy::logger::set_level(baidu_t5_proxy::logger::parse_level(&args.log_level));

    let nodes = match config::load_nodes(args.config.as_deref(), args.port_base) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    if nodes.is_empty() {
        eprintln!("no nodes configured (pass --config nodes.txt)");
        return ExitCode::from(1);
    }

    let idle = if args.idle_timeout > 0.0 {
        Some(Duration::from_secs_f64(args.idle_timeout.min(1.0e9)))
    } else {
        None
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    runtime.block_on(async move {
        if args.check {
            check::run_checks(&nodes, &args.probe_host, &args.probe_path).await;
        } else {
            serve(&args, nodes, idle).await;
        }
    });
    ExitCode::SUCCESS
}
