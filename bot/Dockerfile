FROM rust:1.78-slim as builder

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y \
    pkg-config \
    libssl-dev \
    ffmpeg

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y \
    ffmpeg \
    ca-certificates

WORKDIR /app
COPY --from=builder /app/target/release/discord-llm-bot .

CMD ["./discord-llm-bot"]
