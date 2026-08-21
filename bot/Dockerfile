# Builder stage
FROM rust:1.75-slim as builder

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y \
        ffmpeg \
        pkg-config \
        libssl-dev \
        libsqlite3-dev

WORKDIR /app

COPY Cargo.toml Cargo.lock* ./

RUN mkdir src && \
    echo "fn main() { println!(\"dummy\"); }" > src/main.rs && \
    cargo build --release

COPY src ./src

RUN cargo build --release

# Final stage
FROM debian:bookworm-slim

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y \
        ffmpeg \
        ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/bot /usr/local/bin/bot

WORKDIR /app

CMD ["./bot"]
