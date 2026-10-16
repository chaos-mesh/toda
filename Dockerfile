FROM rust:1.99.1-bookworm

ENV DEBIAN_FRONTEND=noninteractive

ARG HTTPS_PROXY
ARG HTTP_PROXY

ENV http_proxy=${HTTP_PROXY}
ENV https_proxy=${HTTPS_PROXY}

RUN apt-get update && apt-get install -y --no-install-recommends fuse3 && rm -rf /var/lib/apt/lists/*

WORKDIR /toda-build
COPY . .

ENV RUSTFLAGS="-C relro-level=full"
RUN --mount=type=cache,target=/toda-build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --locked --release && cp target/release/toda /toda
