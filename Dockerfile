FROM rust:1.45.0-alpine3.12

ARG HTTPS_PROXY
ARG HTTP_PROXY

RUN apk add --no-cache fuse fuse-dev musl-dev patchelf

RUN rustup toolchain install nightly-2020-07-29
RUN rustup default nightly-2020-07-29

WORKDIR /toda-build
COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock
RUN mkdir src/
RUN echo "fn main() {println!(\"if you see this, the build broke\")}" > src/main.rs
RUN cargo build --release
RUN rm target/release/deps/toda*

COPY . .

RUN cargo build --release

RUN cp /toda-build/target/release/toda /toda