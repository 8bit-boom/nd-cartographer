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
COPY crates/ndcart-server/Cargo.toml crates/ndcart-server/Cargo.toml
RUN mkdir -p crates/ndcart-core/src crates/ndcart-server/src \
    && echo "" > crates/ndcart-core/src/lib.rs \
    && echo "fn main() {}" > crates/ndcart-server/src/main.rs \
    && cargo build --release --locked --target x86_64-unknown-linux-musl

# Real sources.
COPY crates crates
COPY ui ui
RUN touch crates/ndcart-core/src/lib.rs crates/ndcart-server/src/main.rs \
    && cargo build --release --locked --target x86_64-unknown-linux-musl \
    && cp target/x86_64-unknown-linux-musl/release/ndcart /out/ndcart

FROM debian:stable-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/ndcart /usr/local/bin/ndcart
ENV ND_CARTO_LISTEN=0.0.0.0:8300 \
    ND_CARTO_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 8300
CMD ["/usr/local/bin/ndcart", "serve"]
