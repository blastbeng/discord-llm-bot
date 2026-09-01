#!/usr/bin/env bash
# Download the voiceclone sidecar models into ./models (bind-mounted into the
# voiceclone container at /app/models, read-only).
#
#   PocketTTS int8  (~194MB) — zero-shot voice cloning (TTS)
#   whisper-tiny int8 (~245MB, multilingual) — speech-to-text for eavesdrop
#
# Both are only needed once; the compose service mounts ./models read-only.
#
# Usage: ./setup-models.sh [--force] [<model>]
#   --force   re-download even if the model directory already exists
#   <model>   optional: only "pocket-tts" or "whisper-tiny"

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODELS_DIR="$SCRIPT_DIR/models"
mkdir -p "$MODELS_DIR"

FORCE=0
ONLY=""
for arg in "$@"; do
    case "$arg" in
        --force) FORCE=1 ;;
        pocket-tts|whisper-tiny) ONLY="$arg" ;;
        *) echo "Unknown argument: $arg (valid: --force, pocket-tts, whisper-tiny)"; exit 1 ;;
    esac
done

# Downloads and extracts a sherpa-onnx release tarball, verifying the archive
# is complete (tar -t) before extracting — protects against truncated files
# from flaky connections (which once poisoned a Docker remote-ADD cache).
fetch_model() {
    local name="$1" url="$2" dir="$3"
    if [ -d "$MODELS_DIR/$dir" ] && [ "$FORCE" != "1" ]; then
        echo "✓ $dir already present in $MODELS_DIR (use --force to re-download)"
        return 0
    fi
    echo "Downloading $name ..."
    local tmp="/tmp/$dir.tar.bz2"
    curl -fL --retry 3 --progress-bar -o "$tmp" "$url"
    tar -tjf "$tmp" > /dev/null   # integrity check
    tar -xjf "$tmp" -C "$MODELS_DIR"
    rm -f "$tmp"
    echo "✓ $dir installed ($(du -sh "$MODELS_DIR/$dir" | cut -f1))"
}

if [ -z "$ONLY" ] || [ "$ONLY" = "pocket-tts" ]; then
    fetch_model "PocketTTS int8 (voice cloning)" \
        "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/sherpa-onnx-pocket-tts-int8-2026-01-26.tar.bz2" \
        "pocket-tts"
fi

if [ -z "$ONLY" ] || [ "$ONLY" = "whisper-tiny" ]; then
    fetch_model "whisper-tiny int8 (STT, multilingual)" \
        "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-tiny.tar.bz2" \
        "whisper-tiny"
fi

echo ""
echo "Models ready in $MODELS_DIR. Start the stack with:"
echo "  sudo systemctl start docker-compose@discord-llm-bot  (or: docker compose up -d)"