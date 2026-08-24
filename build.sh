#!/usr/bin/env bash
# Build script for the Discord TTS bot using cargo (no Docker required)
# Usage:
#   ./build.sh          - Build in debug mode
#   ./build.sh release   - Build in release mode (optimized)
#   ./build.sh clean     - Clean build artifacts and rebuild

set -euo pipefail

# Ensure cargo is in PATH
export PATH="$HOME/.cargo/bin:$PATH"

# Project root directory (parent of this script's location)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$SCRIPT_DIR/bot"

# Parse arguments
MODE="debug"
DO_CLEAN=false
for arg in "$@"; do
    case "$arg" in
        release|--release|-r)
            MODE="release"
            ;;
        clean|--clean|-c)
            DO_CLEAN=true
            ;;
        *)
            echo "Unknown argument: $arg"
            echo "Usage: $0 [release] [clean]"
            exit 1
            ;;
    esac
done

echo "=== Discord TTS Bot - Cargo Build ==="
echo "Mode: $MODE"
echo "Project: $PROJECT_DIR"
echo ""

# Verify cargo is available
if ! command -v cargo &>/dev/null; then
    echo "ERROR: cargo not found. Install Rust via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

echo "Rust version: $(rustc --version)"
echo "Cargo version: $(cargo --version)"
echo ""

# Clean if requested
if [ "$DO_CLEAN" = true ]; then
    echo "Cleaning build artifacts..."
    (cd "$PROJECT_DIR" && cargo clean)
    echo "Clean complete."
    echo ""
fi

# Build the project
echo "Building (mode: $MODE)..."
if [ "$MODE" = "release" ]; then
    (cd "$PROJECT_DIR" && cargo build --release 2>&1)
else
    (cd "$PROJECT_DIR" && cargo build 2>&1)
fi

BUILD_EXIT=$?

if [ $BUILD_EXIT -ne 0 ]; then
    echo ""
    echo "ERROR: Build failed with exit code $BUILD_EXIT"
    exit $BUILD_EXIT
fi

echo ""
echo "=== Build successful! ==="

# Show the binary location
if [ "$MODE" = "release" ]; then
    BINARY="$PROJECT_DIR/target/release/discord-llm-bot"
else
    BINARY="$PROJECT_DIR/target/debug/discord-llm-bot"
fi

if [ -f "$BINARY" ]; then
    SIZE=$(du -h "$BINARY" | cut -f1)
    echo "Binary: $BINARY ($SIZE)"
else
    echo "WARNING: Binary not found at expected location: $BINARY"
fi