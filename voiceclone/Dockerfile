# =============================================================================
# Stage 1: Build — the voiceclone sidecar with the sherpa-onnx static lib
# =============================================================================
# The sherpa-onnx crate downloads a prebuilt static onnxruntime + sherpa-onnx
# C++ library at build time (no system deps needed beyond a C/C++ toolchain).
FROM rust:1.96-slim AS builder

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    cmake \
    make \
    git \
    binutils \
    curl \
    g++ \
    libstdc++-13-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
# Pre-created dummy main so dependency compilation is cacheable, then real build
RUN mkdir -p src && cargo build --release || cargo build --release
COPY src ./src
RUN cargo build --release && test -f target/release/voiceclone

# =============================================================================
# Stage 2: Runtime — binary + the int8 PocketTTS model files
# =============================================================================
FROM debian:trixie-slim

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ca-certificates \
    procps \
    curl \
    bzip2 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/voiceclone .
COPY entrypoint.sh .
RUN chmod +x entrypoint.sh

# Models are NOT baked into the image: they live on the host in ./models and
# are bind-mounted to /app/models by docker-compose.yml. The entrypoint
# auto-downloads any missing model on container start (self-healing), so
# there is no manual setup step; ./setup-models.sh on the host remains
# available as an offline alternative. This keeps the image small and makes
# model updates independent of image builds.
RUN mkdir -p /app/audios /app/config

ENV VOICECLONE_MODEL_DIR=/app/models/pocket-tts
ENV VOICECLONE_STT_MODEL_DIR=/app/models/whisper-tiny
ENV VOICECLONE_STT_LANG=it
ENV VOICECLONE_STT_THREADS=3
ENV AUDIOS_DIR=/app/audios
ENV DOTENV_PATH=/app/.env

ENTRYPOINT ["./entrypoint.sh"]