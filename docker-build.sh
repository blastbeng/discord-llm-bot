#!/usr/bin/env bash
# Docker image management script for the discord-llm-bot stack.
#
# The services pull their images from Docker Hub (docker-compose.yml uses
# pull_policy: pull and has no build sections). This script gives you control
# over that workflow, and does everything ONE container at a time because we
# host on a slow Raspberry Pi 5 (parallel builds thrash CPU/memory).
#
# Build order: discord-llm-bot -> telegram-bot (whatsapp services temporarily disabled)
#
# Usage:
#   ./docker-build.sh                 Pull the latest images from Docker Hub
#   ./docker-build.sh --force-local   Build images locally from source, then push to Docker Hub
#   ./docker-build.sh --force-local --no-cache   Force a full local rebuild (ignore cache)
#   ./docker-build.sh <service>       Pull only the given service (or build it with --force-local)
#
# NOTE: pushing to Docker Hub requires `docker login` first (non-interactive).
#
# The order of the SERVICES array is the processing order when handling all services.

set -euo pipefail

# Force sequential (non-parallel) behaviour for compose operations too, which
# reduces load on the RPi5 during build/up/down.
export COMPOSE_PARALLEL_LIMIT=1
# Use BuildKit for caching and faster incremental builds.
export DOCKER_BUILDKIT=1
export COMPOSE_DOCKER_CLI_BUILD=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Processing order: discord -> telegram.
# Temporarily disabled (uncomment to re-enable, matching the compose files):
#   whatsapp-bridge whatsapp-bot
SERVICES=(discord-llm-bot telegram-bot)

BUILD_ARGS=()
FORCE_LOCAL=0
NO_CACHE=0
SELECTED=""

for arg in "$@"; do
    case "$arg" in
        --force-local)
            FORCE_LOCAL=1
            ;;
        --no-cache)
            NO_CACHE=1
            ;;
        --help|-h)
            echo "Usage: $0 [--force-local] [--no-cache] [<service>]"
            echo ""
            echo "  (no flags)     Pull the latest images from Docker Hub"
            echo "  --force-local  Build images locally from source, then push to Docker Hub"
            echo "  --no-cache     Force a full rebuild ignoring the build cache"
            echo ""
            echo "Services: ${SERVICES[*]}"
            exit 0
            ;;
        *)
            SELECTED="$arg"
            ;;
    esac
done

if [ -n "$SELECTED" ]; then
    if ! printf '%s\n' "${SERVICES[@]}" | grep -qx "$SELECTED"; then
        echo "ERROR: unknown service '$SELECTED'."
        echo "Valid services: ${SERVICES[*]}"
        exit 1
    fi
    SERVICES=("$SELECTED")
fi

[ "$NO_CACHE" = "1" ] && BUILD_ARGS+=(--no-cache)

echo "=== docker-build.sh ==="
echo "Services: ${SERVICES[*]}"
echo "Mode: $([ "$FORCE_LOCAL" = "1" ] && echo 'LOCAL BUILD + PUSH' || echo 'PULL FROM DOCKER HUB')"
echo "Parallel limit: $COMPOSE_PARALLEL_LIMIT (sequential)"
echo ""

if [ "$FORCE_LOCAL" = "1" ]; then
    # ---- Local build + push (one service at a time) ----
    COMPOSE_FILES="-f docker-compose.yml -f docker-compose.build.yml"
    for svc in "${SERVICES[@]}"; do
        echo "----------------------------------------------------------------"
        echo ">>> Building $svc locally ..."
        echo "----------------------------------------------------------------"
        # shellcheck disable=SC2086
        docker compose $COMPOSE_FILES build "$svc" ${BUILD_ARGS[@]}
        echo ""
    done

    echo "=== Pushing images to Docker Hub ==="
    # shellcheck disable=SC2086
    docker compose $COMPOSE_FILES push ${SERVICES[@]}
    echo "=== Push completed ==="
    echo "=== All local builds and pushes completed successfully ==="
else
    # ---- Pull latest from Docker Hub (one service at a time) ----
    for svc in "${SERVICES[@]}"; do
        echo "----------------------------------------------------------------"
        echo ">>> Pulling latest $svc from Docker Hub ..."
        echo "----------------------------------------------------------------"
        docker compose -f docker-compose.yml pull "$svc"
        echo ""
    done
    echo "=== All pulls completed successfully ==="
fi
