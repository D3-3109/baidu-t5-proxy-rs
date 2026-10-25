# baidu-t5-proxy (Rust)

[![CI](https://github.com/D3-3109/baidu-t5-proxy-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/D3-3109/baidu-t5-proxy-rs/actions/workflows/ci.yml)
[![Release](https://github.com/D3-3109/baidu-t5-proxy-rs/actions/workflows/release.yml/badge.svg)](https://github.com/D3-3109/baidu-t5-proxy-rs/actions/workflows/release.yml)

跑在 VPS 上的“一端口一节点”轻量出口代理：每个本地监听端口绑定一个百度 T5
节点 IP，本机服务把代理指向对应端口，出口 IP 就是该节点的出口。

本仓库是 [D3-3109/baidu-t5-proxy](https://github.com/D3-3109/baidu-t5-proxy)
（Python 版）的 **Rust 完全重写**：功能、命令行参数、节点表格式、日志格式
与 Python 版保持一致，语义逐项对齐（含回归测试逐条移植）。

```
本机服务 ──socks5/http──▶ 127.0.0.1:1000x ──CONNECT──▶ 节点IP:443 ──▶ 目标
```

依赖仅 `tokio`（异步运行时）+ `chrono`（本地时间戳），编译为单个静态二进制，
无解释器、无运行时依赖。CI/CD 由 GitHub Actions 提供：
`cargo test/clippy/fmt` 守护合并；打 `v*` 标签自动发布 Linux x86_64/aarch64
静态二进制与 Windows 二进制到 Releases，并构建 linux/amd64 + linux/arm64
Docker 镜像推送到 GHCR。

## 协议原理（逆向自百度 T5 加速通道）

T5 节点是**明文 HTTP CONNECT 代理**（端口 443 但没有 TLS）：

```
CONNECT <真实目标>:<端口> HTTP/1.1
Host: ascdn.baidu.com
Proxy-Connection: Keep-Alive
X-T5-Auth: 1951164069
User-Agent: okhttp/3.11.0 ... baiduboxapp/13.33.0.11 (Baidu; P1 9)
```

要点：

- 请求行写**真实目标**，`Host` 头固定为 `ascdn.baidu.com`（路由靠请求行）。
- `X-T5-Auth` 是鉴权 token，缺失会被 403。
- 收到 2xx 后该连接即为到目标的透明隧道；**不支持绝对 URI 转发**，所以
  明文 HTTP 请求也会在内部先 CONNECT 再重放。
- 目标域名由节点侧解析（出口视角 DNS，与出口 IP 地域一致）。

## 安装

### 方式一：GitHub Releases 下载（推荐，Linux VPS）

到 [Releases](https://github.com/D3-3109/baidu-t5-proxy-rs/releases) 下载对应
架构的压缩包（musl 静态链接，任何发行版可直接运行，无依赖）：

```bash
# x86_64
curl -LO https://github.com/D3-3109/baidu-t5-proxy-rs/releases/latest/download/baidu-t5-proxy-x.y.z-x86_64-unknown-linux-musl.tar.gz
tar xzf baidu-t5-proxy-*.tar.gz && cd baidu-t5-proxy-*
```

### 方式二：Docker 镜像（GHCR，amd64 + arm64）

```bash
mkdir -p /opt/baidu-t5-proxy && cd /opt/baidu-t5-proxy
# 编辑 nodes.txt 后：
docker run -d --name baidu-t5-proxy --restart always \
  -v ./nodes.txt:/etc/baidu-t5-proxy/nodes.txt:ro \
  -p 127.0.0.1:10001-10014:10001-10014 \
  ghcr.io/d3-3109/baidu-t5-proxy-rs:latest

# 体检节点（容器内自带 fake-baidu 也可用于本地调试）
docker run --rm -v ./nodes.txt:/etc/baidu-t5-proxy/nodes.txt:ro \
  ghcr.io/d3-3109/baidu-t5-proxy-rs:latest --check
```

或使用 compose（`docker compose up -d`）：

```yaml
services:
  baidu-t5-proxy:
    image: ghcr.io/d3-3109/baidu-t5-proxy-rs:latest
    restart: always
    volumes:
      - ./nodes.txt:/etc/baidu-t5-proxy/nodes.txt:ro
    ports:
      - "127.0.0.1:10001-10014:10001-10014"
```

### 方式三：源码构建

需要 Rust 1.71+：

```bash
cargo build --release
# 产物：
#   target/release/baidu-t5-proxy    主代理
#   target/release/fake-baidu        本地自测用的假 T5 节点
```

## 快速开始

```bash
mkdir -p /opt/baidu-t5-proxy && cd /opt/baidu-t5-proxy
# 上传 baidu-t5-proxy(target/release) nodes.txt 到这里

# 1. 先体检节点（显示每个节点是否可用 + 出口 IP）
./baidu-t5-proxy --config nodes.txt --check

# 2. 前台跑起来试试
./baidu-t5-proxy --config nodes.txt

# 3. 验证出口 IP
curl -sx socks5h://127.0.0.1:10001 https://myip.ipip.net/
curl -sx http://127.0.0.1:10001   https://myip.ipip.net/
```

## 节点表格式（nodes.txt）

```
<本地监听端口> <节点IP> [节点端口，默认443]
```

只写 IP 时监听端口从 `--port-base`（默认 10001）自动递增。改完记得 `--check`。

## systemd 常驻

```bash
cp baidu-t5-proxy.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now baidu-t5-proxy
journalctl -u baidu-t5-proxy -f
```

## 给服务使用

```bash
# 环境变量方式（多数程序通用）
export HTTP_PROXY=http://127.0.0.1:10001
export HTTPS_PROXY=http://127.0.0.1:10001
# 或 SOCKS5
export ALL_PROXY=socks5h://127.0.0.1:10001

# curl 单次
curl -x socks5h://127.0.0.1:10001 https://target.example/
```

Docker 里的服务可用 `--listen 0.0.0.0` 后经宿主网关（如 172.17.0.1）访问，
注意配合防火墙规则，不要把端口暴露到公网。

## 本地自测（不消耗真实节点）

```bash
target/release/fake-baidu 127.0.0.1 44444 &
echo "10002 127.0.0.1 44444" > /tmp/nodes-test.txt
target/release/baidu-t5-proxy --config /tmp/nodes-test.txt
curl -sx socks5h://127.0.0.1:10002 https://myip.ipip.net/
```

一键回归自检（自动起 fake 节点 + 代理实例，跑完即退，不含真实节点）：

```bash
cargo test --test regression -- --nocapture
# 覆盖 CONNECT 早到数据 / 明文 HTTP 单请求 / SOCKS5 协商等 13 项
```

## 其他参数

| 参数 | 说明 |
| --- | --- |
| `--listen 127.0.0.1` | 监听地址，默认仅本机 |
| `--port-base 10001` | 未写端口的节点行从这里自动分配 |
| `--idle-timeout 900` | 隧道空闲超时（秒），0 关闭 |
| `--check` | 体检所有节点并退出 |
| `--log-level DEBUG` | 打印每条连接与流量统计 |
| 环境变量 `T5_AUTH` / `T5_UA` / `T5_HOST` | token 失效时热更新握手参数 |

日志写到 stderr，格式 `HH:MM:SS LEVEL message`，时间为本机时区。

## 与 Python 版的差异

- 日志时间戳同样取本机时区；实现上依赖 `chrono`。
- 极少数非法输入（如 `[::1]:abc`、nodes.txt 里端口写非数字）Python 版会以
  未捕获异常崩溃，Rust 版统一按配置/协议错误干净退出，行为更严格。
- 并发模型从 asyncio 换成 tokio 多线程运行时，其余语义（协议识别、早到数据
  透传、半关闭传播、逐请求重放、CRLF/超大头防护）逐项对齐，并由
  `tests/regression.rs`（对应 Python 版 `verify_fixes.py` 的 13 项用例）把关。

## CI/CD

| 工作流 | 触发 | 内容 |
| --- | --- | --- |
| `ci.yml` | push 到 main / PR | `cargo fmt --check`、`clippy -D warnings`、`cargo test`（含 13 项回归）、Dockerfile 构建验证 |
| `release.yml` | 推送 `v*` 标签 | 交叉编译 `x86_64/aarch64-unknown-linux-musl` + `x86_64-pc-windows-msvc` 二进制并打包上传 GitHub Release；`linux/amd64` + `linux/arm64` 镜像推送 GHCR（`x.y.z`、`x.y`、`latest` 标签） |

发版流程：改完代码后 `git tag v1.x.y && git push origin v1.x.y`，其余自动。

## 注意事项

- 仅支持 TCP 转发（T5 本身只支持 CONNECT），没有 UDP。
- token 来自百度客户端内部接口，可能随版本更新失效；节点也可能限速、限制
  目标端口或不可用，出现大面积失败时先用 `--check` 排查，再考虑换 token/UA。
- 该接口属非公开服务，稳定性与合规风险请自行评估，建议低频、少量业务使用。
