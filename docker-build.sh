#!/usr/bin/env bash
# Sequential Docker build script for the discord-llm-bot stack.
#
# We host on a slow Raspberry Pi 5, so `docker compose build` (which builds all
# services in parallel) thrashes CPU/memory and makes every rebuild painfully
# slow. This script builds each service ONE AT A TIME in a fixed order:
#
#   discord-llm-bot -> telegram-bot -> whatsapp-bridge -> whatsapp-bot
#
# Each service is built individually (which is inherently sequential: one
# command blocks until the previous finishes), and BuildKit caching is enabled
# so the cargo-chef dependency layers are reused between rebuilds.
#
# Usage:
#   ./docker-build.sh                 Build all services (with cache)
#   ./docker-build.sh <service>       Build only the given service
#   ./docker-build.sh --no-cache      Force a full rebuild (ignore cache)
#   ./docker-build.sh --push          Also push built images to the registry
#
# The order of the SERVICES array is the build order when building everything.

set -euo pipefail

# Force sequential (non-parallel) behaviour for compose operations too, which
# further reduces load on the RPi5 during build/up/down.
export COMPOSE_PARALLEL_LIMIT=1
# Use BuildKit for caching and faster incremental builds.
export DOCKER_BUILDKIT=1
export COMPOSE_DOCKER_CLI_BUILD=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Build order: discord -> telegram -> whatsapp-bridge -> whatsapp-bot
SERVICES=(discord-llm-bot telegram-bot whatsapp-bridge whatsapp-bot)

EXTRA_ARGS=()
PUSH=0
NO_CACHE=0
SELECTED=""

for arg in "$@"; do
    case "$arg" in
        --push)
            PUSH=1
            ;;
        --no-cache)
            NO_CACHE=1
            ;;
        --help|-h)
            echo "Usage: $0 [--no-cache] [--push] [<service>]"
            echo "Services: ${SERVICES[*]}"
            exit 0
            ;;
        *)
            SELECTED="$arg"
            ;;
    esac
done

[ "$NO_CACHE" = "1" ] && EXTRA_ARGS+=(--no-cache)
[ "$PUSH" = "1" ] && EXTRA_ARGS+=(--push)

if [ -n "$SELECTED" ]; then
    # Validate the requested service name.
    if ! printf '%s\n' "${SERVICES[@]}" | grep -qx "$SELECTED"; then
        echo "ERROR: unknown service '$SELECTED'."
        echo "Valid services: ${SERVICES[*]}"
        exit 1
    fi
    SERVICES=("$SELECTED")
fi

echo "=== Sequential docker build ==="
echo "Services to build: ${SERVICES[*]}"
echo "Extra args: ${EXTRA_ARGS[*]:-none}"
echo "Parallel limit: $COMPOSE_PARALLEL_LIMIT (sequential)"
echo ""

for svc in "${SERVICES[@]}"; do
    echo "----------------------------------------------------------------"
    echo ">>> Building $svc ..."
    echo "----------------------------------------------------------------"
    # shellcheck disable=SC2068
    docker compose -f docker-compose.yml build "$svc" ${EXTRA_ARGS[@]}
    echo ""
done

echo "=== All requested builds completed successfully ==="
