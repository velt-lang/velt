#!/usr/bin/env bash
# Prints the part of the rust-cache key that changes when the dependencies' compiled output does:
# a hash of rust-toolchain.toml (the pinned compiler) and one of the external packages in
# Cargo.lock. Nothing else: not the toolchains a runner image has installed besides the pinned
# one, not the workspace's own versions (a release bumps them), not the manifests. The setup
# action keys the cache with it, and the nightly `cache-prune` job deletes the entries of any
# other key. Run from the repository root.
set -euo pipefail
toolchain=$(git hash-object rust-toolchain.toml | cut -c1-8)
# The [[package]] entries with a `source` (crates.io, git); the workspace's own have none.
deps=$(awk '
  /^\[\[package\]\]/ { if (keep) printf "%s", block; block = ""; keep = 0 }
  { block = block $0 "\n" }
  /^source = / { keep = 1 }
  END { if (keep) printf "%s", block }
' Cargo.lock | git hash-object --stdin | cut -c1-8)
echo "gate-$toolchain-$deps"
