#!/bin/sh
# Entrypoint for the voiceclone sidecar.
#
# The models are expected in /app/models (bind-mounted from the host's
# ./models). If a model is missing (fresh checkout, new host), it is
# downloaded automatically ON CONTAINER START — no manual ./setup-models.sh
# step required, and restarts are fully self-healing. Downloads and
# extraction happen in /tmp and the result is moved into the mount once
# complete, so a crash mid-download never leaves a broken model behind.
#
# If the fetch fails (read-only mount, no network), the service still starts
# — the missing feature (cloning or STT) reports an error at request time
# while the other keeps working.
set -u

fetch() {
    # $1 = model name, $2 = expected top-level dir inside the tarball,
    # $3 = tarball URL. The model installs to /app/models/$1.
    dir="/app/models/$1"
    # Already present? (accept whatever the mount provides)
    if [ -e "$dir" ] && [ -n "$(ls "$dir" 2>/dev/null)" ]; then
        echo "entrypoint: $1 present, skipping download"
        return 0
    fi
    echo "entrypoint: $1 missing, downloading..."
    tmp_tar="/tmp/$1.tar.bz2"
    curl -fsSL --retry 3 -o "$tmp_tar" "$3" || {
        echo "entrypoint: WARNING download failed for $1 (feature will be unavailable)"
        return 0
    }
    # Integrity: a truncated tarball must never be installed.
    tar -tjf "$tmp_tar" > /dev/null 2>&1 || {
        echo "entrypoint: WARNING corrupt tarball for $1, discarding"
        rm -f "$tmp_tar"
        return 0
    }
    rm -rf "/tmp/$2"
    tar -xjf "$tmp_tar" -C /tmp || {
        echo "entrypoint: WARNING extraction failed for $1"
        rm -f "$tmp_tar"
        return 0
    }
    rm -f "$tmp_tar"
    if mv "/tmp/$2" "$dir" 2>/dev/null; then
        echo "entrypoint: $1 installed"
    elif mkdir -p "$dir" 2>/dev/null && cp -r "/tmp/$2/." "$dir/" 2>/dev/null; then
        echo "entrypoint: $1 installed (copied into read-only-ish mount)"
        rm -rf "/tmp/$2"
    else
        echo "entrypoint: WARNING could not install $1 into the models mount"
        rm -rf "/tmp/$2"
    fi
}

fetch pocket-tts \
    "sherpa-onnx-pocket-tts-int8-2026-01-26" \
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/sherpa-onnx-pocket-tts-int8-2026-01-26.tar.bz2"

fetch whisper-tiny \
    "sherpa-onnx-whisper-tiny" \
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-tiny.tar.bz2"

# Hand over to the service (exec so PID 1 receives signals properly).
exec ./voiceclone