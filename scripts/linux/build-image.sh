#!/usr/bin/env bash
# Build a Docker image for one platform and check that it really is for that platform.
# Usage: scripts/linux/build-image.sh <platform> <tag> <dockerfile> [docker build args...]
# Uses buildx when the docker CLI has it: the legacy builder ignores --platform and silently
# builds for the host (an "amd64" image that is arm64 on Apple silicon).
set -euo pipefail

platform=$1
tag=$2
dockerfile=$3
shift 3
context=$(dirname "$dockerfile")
if docker buildx version >/dev/null 2>&1; then
    docker buildx build -q --load --platform "$platform" -t "$tag" -f "$dockerfile" "$@" \
        "$context" >/dev/null
else
    docker build -q --platform "$platform" -t "$tag" -f "$dockerfile" "$@" "$context" >/dev/null
fi
want=${platform#linux/}
got=$(docker image inspect "$tag" --format '{{.Architecture}}')
if [ "$got" != "$want" ]; then
    echo "error: $tag was built for $got, not $want; install the docker buildx plugin" \
        "(OrbStack: ln -s /Applications/OrbStack.app/Contents/MacOS/xbin/docker-buildx" \
        "~/.docker/cli-plugins/docker-buildx)" >&2
    exit 1
fi
