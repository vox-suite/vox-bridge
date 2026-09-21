FROM rust:1.91-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y pkg-config libssl-dev g++ && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl libstdc++6 && rm -rf /var/lib/apt/lists/*
RUN useradd -m -u 1000 -U vox
USER vox

COPY --from=builder --chown=vox:vox /app/target/release/vox-bridge /usr/local/bin/vox-bridge
EXPOSE 3000
CMD ["/usr/local/bin/vox-bridge"]
