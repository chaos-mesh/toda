# syntax=docker/dockerfile:experimental

FROM rust:1.45.0-alpine3.12

ARG HTTPS_PROXY
ARG HTTP_PROXY

RUN apk add --no-cache fuse fuse-dev musl-dev patchelf

WORKDIR /toda-build

COPY . .

RUN --mount=type=cache,target=/usr/local/rustup \
    --mount=type=cache,target=/toda-build/target \
    cargo build --release

RUN --mount=type=cache,target=/toda-build/target \
    cp /toda-build/target/release/toda /toda