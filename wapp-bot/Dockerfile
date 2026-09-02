FROM rust:1.89-slim AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    cmake \
    make \
    git \
    binutils \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY Cargo.toml Cargo.lock* ./
COPY src ./src

RUN cargo build --release
RUN test -f target/release/wapp-bot

FROM debian:trixie-slim

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ffmpeg \
    praat \
    ca-certificates \
    procps \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/wapp-bot .

RUN mkdir -p /app/audios /app/config

CMD ["./wapp-bot"]