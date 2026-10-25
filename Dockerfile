# syntax=docker/dockerfile:1

# ---- 构建阶段：官方 rust:alpine（musl），产物为静态链接二进制 ----
FROM rust:1-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app

# 依赖层：先用占位源码把依赖编好，源码变动时不触发全量重编
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin tests \
    && echo "" > src/lib.rs \
    && echo "fn main() {}" > src/main.rs \
    && echo "fn main() {}" > src/bin/fake_baidu.rs \
    && cargo build --release \
    && rm -rf src

# 真实源码。占位编译的 cargo 指纹会让真实源码被误判为"最新"
# （clean -p 实测也清不掉），因此直接删除本包产物与指纹强制重编。
COPY src ./src
COPY tests ./tests
RUN set -x \
    && ls target/release/.fingerprint/ | head -30 \
    && rm -f target/release/baidu-t5-proxy target/release/fake-baidu \
    && rm -rf target/release/.fingerprint/baidu-t5-proxy-* \
              target/release/.fingerprint/baidu_t5_proxy-* \
              target/release/.fingerprint/fake-baidu-* \
              target/release/.fingerprint/fake_baidu-* \
    && cargo build --release \
    && grep -aq "usage: baidu-t5-proxy" target/release/baidu-t5-proxy \
    && cp target/release/baidu-t5-proxy target/release/fake-baidu /usr/local/bin/

# ---- 运行阶段：distroless 静态镜像，非 root 运行 ----
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=builder /usr/local/bin/baidu-t5-proxy /usr/local/bin/baidu-t5-proxy
COPY --from=builder /usr/local/bin/fake-baidu /usr/local/bin/fake-baidu
COPY nodes.txt /etc/baidu-t5-proxy/nodes.txt

# 节点表可挂载覆盖：-v ./nodes.txt:/etc/baidu-t5-proxy/nodes.txt:ro
EXPOSE 10001-10014

ENTRYPOINT ["/usr/local/bin/baidu-t5-proxy"]
CMD ["--config", "/etc/baidu-t5-proxy/nodes.txt", "--listen", "0.0.0.0"]
