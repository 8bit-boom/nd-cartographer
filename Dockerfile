# nd-cartographer — the browser/docker image (plan §11.2).
# Same pattern as nd-world's Dockerfile.gateway: stub-source dependency
# layer caching, then a real build; slim runtime.

FROM rust:1-slim AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl
WORKDIR /src

# Cache the dependency build with stub sources.
COPY Cargo.toml Cargo.lock ./
COPY crates/ndcart-core/Cargo.toml crates/ndcart-core/Cargo.toml
COPY crates/ndcart-formats/Cargo.toml crates/ndcart-formats/Cargo.toml
COPY crates/ndcart-bridge/Cargo.toml crates/ndcart-bridge/Cargo.toml
COPY crates/ndcart-paint/Cargo.toml crates/ndcart-paint/Cargo.toml
COPY crates/ndcart-terrain/Cargo.toml crates/ndcart-terrain/Cargo.toml
COPY crates/ndcart-server/Cargo.toml crates/ndcart-server/Cargo.toml
RUN mkdir -p crates/ndcart-core/src crates/ndcart-formats/src crates/ndcart-bridge/src crates/ndcart-paint/src crates/ndcart-terrain/src crates/ndcart-server/src \
    && echo "" > crates/ndcart-core/src/lib.rs \
    && echo "" > crates/ndcart-formats/src/lib.rs \
    && echo "" > crates/ndcart-bridge/src/lib.rs \
    && echo "" > crates/ndcart-paint/src/lib.rs \
    && echo "" > crates/ndcart-terrain/src/lib.rs \
    && echo "fn main() {}" > crates/ndcart-server/src/main.rs \
    && cargo build --release --locked --target x86_64-unknown-linux-musl

# Real sources.
COPY crates crates
COPY ui ui
RUN touch crates/ndcart-core/src/lib.rs crates/ndcart-formats/src/lib.rs crates/ndcart-bridge/src/lib.rs crates/ndcart-paint/src/lib.rs crates/ndcart-terrain/src/lib.rs crates/ndcart-server/src/main.rs \
    && cargo build --release --locked --target x86_64-unknown-linux-musl \
    && mkdir -p /out && cp target/x86_64-unknown-linux-musl/release/ndcart /out/ndcart

FROM debian:stable-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/ndcart /usr/local/bin/ndcart
# Runs as an unprivileged user (568 = the "apps" user on TrueNAS SCALE). If your
# dataset has another owner, set `user:` in compose (docs/DOCKER.md).
RUN useradd --system --uid 568 --user-group --home-dir /data --shell /usr/sbin/nologin ndcart \
    && mkdir -p /data && chown ndcart:ndcart /data
USER 568:568
ENV ND_CARTO_LISTEN=0.0.0.0:8300 \
    ND_CARTO_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 8300
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["/usr/local/bin/ndcart", "healthcheck"]
CMD ["/usr/local/bin/ndcart", "serve"]
