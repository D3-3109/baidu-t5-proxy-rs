//! baidu-t5-proxy — “一端口一节点”的百度 T5 出口代理（Rust 实现）。
//!
//! 流量路径:
//!
//! ```text
//! 本机服务 --(SOCKS5 / HTTP 代理)--> 127.0.0.1:PORT --> 节点IP:443 --CONNECT--> 目标
//! ```
//!
//! 特性:
//!   * 每个本地监听端口绑定一个上游节点 IP（节点表见 nodes.txt）。
//!   * 同一端口同时支持 SOCKS5(CONNECT) 与 HTTP(CONNECT + 绝对URI 请求)，按首字节
//!     自动识别：0x05 = SOCKS5，其余按 HTTP 解析。
//!   * 上游固定走 T5 CONNECT 握手（T5 不支持绝对URI转发），目标域名由节点侧解析，
//!     出口 DNS 与出口 IP 一致。
//!   * 仅依赖 tokio + chrono，无其他第三方依赖。

pub mod check;
pub mod config;
pub mod inbound;
pub mod logger;
pub mod proxy;
pub mod t5;
pub mod util;
