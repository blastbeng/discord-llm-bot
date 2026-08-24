# =============================================================================
# Stage 1: Plan — generate cargo-chef recipe from Cargo.toml
# =============================================================================
FROM rust:1.88-slim AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo chef prepare --recipe-path recipe.json

# =============================================================================
# Stage 2: Cook — compile all dependencies (cached unless Cargo.toml deps change)
# =============================================================================
FROM chef AS builder

# Install build dependencies
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

# Cook dependencies from the recipe — this layer is cached as long as
# the dependency set (Cargo.toml) doesn't change
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# =============================================================================
# Stage 3: Build — compile the actual application (only recompiles your code)
# =============================================================================
COPY Cargo.toml ./
COPY src ./src

# Build the real binary; dependencies are already compiled in the cook layer
RUN cargo build --release

# Verify the binary is real (not a dummy)
RUN test -f target/release/discord-llm-bot

# =============================================================================
# Stage 4: Runtime — minimal image with only the binary and runtime deps
# =============================================================================
FROM debian:bookworm-slim

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