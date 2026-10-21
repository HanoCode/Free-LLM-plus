# Freeport 的容器镜像：只跑无界面网关，不含任何 GUI 依赖。
#
# 为什么单独编一个 bin：桌面包要 WebKit / 窗口系统，塞进容器是几百 MB 的
# 无用依赖。`freeport-daemon` 只依赖 gateway + mcp，编出来不到 40 MB。
#
# 端口一律由外层发布时收回回环（`-p 127.0.0.1:8787:8787`）。容器内必须绑
# `0.0.0.0` 才能被 docker 转发，所以运行时给了 `FREEPORT_ALLOW_NON_LOOPBACK=1`——
# 那不是「对外开放」，是「允许被 docker 转发」。没有这层发布，容器照样只能
# 在本机被访问。

FROM rust:1-bookworm AS build
WORKDIR /src

# 整个 app/ 一起拷：src-tauri 是 workspace 成员，少了它连 manifest 都解析不了，
# 哪怕我们不编它。node_modules / target / dist 由 .dockerignore 挡掉。
COPY app ./
RUN cargo build --release --locked -p freeport-daemon

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*

# 非 root 跑：这个进程手里有用户的 API key，能少一分权限就少一分
RUN useradd --system --uid 10001 --create-home --home-dir /home/freeport freeport && \
    mkdir -p /data && chown freeport:freeport /data

COPY --from=build /src/target/release/freeport /usr/local/bin/freeport

USER freeport
WORKDIR /home/freeport

ENV FREEPORT_DATA_DIR=/data \
    FREEPORT_PORT=8787 \
    FREEPORT_BIND=0.0.0.0 \
    FREEPORT_ALLOW_NON_LOOPBACK=1 \
    RUST_LOG=info,freeport_probe=warn

VOLUME ["/data"]
EXPOSE 8787

# 用 /health 而不是 /v1/models：后者会碰上游，健康检查不该消耗任何额度
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${FREEPORT_PORT}/health" > /dev/null || exit 1

ENTRYPOINT ["freeport"]
