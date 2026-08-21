FROM rust:1.78-slim as builder

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y \
    pkg-config \
    libssl-dev \
    ffmpeg

WORKDIR /app

# Copy only Cargo.toml first to cache dependencies
COPY Cargo.toml ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release || true
RUN rm -rf src

# Copy the actual source code and build
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
