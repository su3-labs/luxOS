FROM rust:1.99-bookworm AS compile
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY runtime runtime
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates e2fsprogs gdisk util-linux && rm -rf /var/lib/apt/lists/*
COPY --from=compile /source/target/release/luxos /luxos-bin/luxos
