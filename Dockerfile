# syntax=docker/dockerfile:1

# ---------- build ----------
FROM rust:1.98-slim-trixie AS builder

WORKDIR /app

# libsqlite3-sys compiles a bundled SQLite, so the builder needs a C toolchain.
RUN apt-get update \
    && apt-get install -y --no-install-recommends gcc libc6-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations

# The registry and the target directory live in build caches, so a rebuild only
# recompiles what changed. The binary is copied out while the mount is still
# attached -- nothing from a cache mount survives the layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --release --locked \
    && cp target/release/ashtray /usr/local/bin/ashtray

# ---------- run ----------
FROM debian:trixie-slim

# curl is here for the health check and nothing else.
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --no-create-home ashtray \
    && install -d -o ashtray -g ashtray /data

COPY --from=builder /usr/local/bin/ashtray /usr/local/bin/ashtray

# 127.0.0.1 is unreachable from outside a container, and the database file
# belongs on the volume rather than in the container layer.
ENV ASHTRAY_BIND_ADDR=0.0.0.0:8080 \
    ASHTRAY_DATABASE_URL=sqlite:///data/ashtray.db \
    ASHTRAY_LOG=info

USER ashtray
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/health || exit 1

ENTRYPOINT ["/usr/local/bin/ashtray"]
