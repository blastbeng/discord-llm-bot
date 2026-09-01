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
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/voiceclone .

# PocketTTS int8 model (~194MB): downloaded at image build time so the runtime
# image is self-contained and the Pi never downloads from GitHub at runtime.
# To rebuild with a newer model, bump the version and rebuild the service.
ADD https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/sherpa-onnx-pocket-tts-int8-2026-01-26.tar.bz2 /tmp/pocket.tar.bz2
RUN tar -xjf /tmp/pocket.tar.bz2 -C /tmp && \
    mv /tmp/sherpa-onnx-pocket-tts-int8-2026-01-26 /app/models/pocket-tts && \
    rm /tmp/pocket.tar.bz2 && \
    mkdir -p /app/audios /app/config

# Whisper-tiny int8 STT model (~245MB, multilingual): powers the /transcribe
# endpoint used by the eavesdrop feature. Same self-contained approach —
# downloaded and extracted at image build time, never at runtime.
ADD https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-tiny.tar.bz2 /tmp/whisper.tar.bz2
RUN tar -xjf /tmp/whisper.tar.bz2 -C /tmp && \
    mv /tmp/sherpa-onnx-whisper-tiny /app/models/whisper-tiny && \
    rm /tmp/whisper.tar.bz2

ENV VOICECLONE_MODEL_DIR=/app/models/pocket-tts
ENV VOICECLONE_STT_MODEL_DIR=/app/models/whisper-tiny
ENV VOICECLONE_STT_LANG=it
ENV VOICECLONE_STT_THREADS=3
ENV AUDIOS_DIR=/app/audios
ENV DOTENV_PATH=/app/.env

CMD ["./voiceclone"]