FROM rust:1.88-slim AS builder

# Install only necessary build dependencies (libssl-dev not needed due to rustls)
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    cmake \
    make \
    git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Set cargo build jobs to 4 to speed up compilation
ENV CARGO_BUILD_JOBS=4

# Copy only Cargo.toml first to cache dependencies
COPY Cargo.toml ./
RUN mkdir src && echo 'fn main() { eprintln!("DUMMY BINARY - this means the real build was skipped!"); }' > src/main.rs
RUN cargo build --release || true
RUN rm -rf src

# Copy the actual source code and build
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ffmpeg \
    ca-certificates \
    procps \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/discord-llm-bot .

RUN mkdir -p /app/audios /app/config

CMD ["./discord-llm-bot"]
