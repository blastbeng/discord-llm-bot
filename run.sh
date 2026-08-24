#!/usr/bin/env bash
# Run script for the Discord TTS bot using cargo (no Docker required)
# Usage:
#   ./run.sh           - Build (debug) and run
#   ./run.sh release    - Build (release) and run
#   ./run.sh direct     - Run already-built binary directly (no build)
#   ./run.sh directrelease - Run already-built release binary directly

set -euo pipefail

# Ensure cargo is in PATH
export PATH="$HOME/.cargo/bin:$PATH"

# Project root directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$SCRIPT_DIR/bot"

MODE="debug"
DIRECT=false
RELEASE_BINARY=false

for arg in "$@"; do
    case "$arg" in
        release|--release|-r)
            MODE="release"
            ;;
        direct|--direct|-d)
            DIRECT=true
            ;;
        directrelease)
            DIRECT=true
            RELEASE_BINARY=true
            ;;
        *)
            echo "Unknown argument: $arg"
            echo "Usage: $0 [release] [direct] [directrelease]"
            exit 1
            ;;
    esac
done

# Load .env if it exists (cargo run doesn't auto-load it, but the bot does via dotenv)
if [ -f "$SCRIPT_DIR/.env" ]; then
    echo "Found .env file (bot will load it via dotenv)"
fi

# Ensure required directories exist
mkdir -p "$SCRIPT_DIR/config"
mkdir -p "$SCRIPT_DIR/audios"
mkdir -p "$SCRIPT_DIR/tmp/discord-llm-bot"

if [ "$DIRECT" = true ]; then
    # Run pre-built binary directly
    if [ "$RELEASE_BINARY" = true ] || [ "$MODE" = "release" ]; then
        BINARY="$PROJECT_DIR/target/release/discord-llm-bot"
    else
        BINARY="$PROJECT_DIR/target/debug/discord-llm-bot"
    fi

    if [ ! -f "$BINARY" ]; then
        echo "ERROR: Binary not found at $BINARY"
        echo "Run './build.sh' or './build.sh release' first, or use './run.sh' without 'direct'"
        exit 1
    fi

    echo "=== Running pre-built binary ==="
    echo "Binary: $BINARY"
    exec "$BINARY"
else
    # Build and run with cargo
    echo "=== Building and running with cargo (mode: $MODE) ==="

    if [ "$MODE" = "release" ]; then
        (cd "$PROJECT_DIR" && cargo run --release 2>&1)
    else
        (cd "$PROJECT_DIR" && cargo run 2>&1)
    fi
fi