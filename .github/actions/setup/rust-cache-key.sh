#!/usr/bin/env bash
# Prints the part of the rust-cache key that changes when the dependencies' compiled output does:
# a hash of rust-toolchain.toml (the pinned compiler), one of the external packages in Cargo.lock,
# and one of the build settings in the workspace's manifests: every `[profile.*]` section, every
# dependency table (`[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, their
# `[target.*.…]` and `[workspace.…]` forms) and `[features]`, without the workspace's own path
# dependencies, plus .cargo/config.toml. Nothing else: not the toolchains a runner image has
# installed besides the pinned one, not `[package]` (a release bumps the versions), not comments.
# The setup action keys the cache with it, and the nightly `cache-prune` job deletes the entries of
# any other key. Run from the root of a git checkout; crates/xtask/src/cache_key_tests.rs tests it.
set -euo pipefail
toolchain=$(git hash-object rust-toolchain.toml | cut -c1-8)
# The [[package]] entries with a `source` (crates.io, git); the workspace's own have none.
deps=$(awk '
  /^\[\[package\]\]/ { if (keep) printf "%s", block; block = ""; keep = 0 }
  { block = block $0 "\n" }
  /^source = / { keep = 1 }
  END { if (keep) printf "%s", block }
' Cargo.lock | git hash-object --stdin | cut -c1-8)
# The build settings of the workspace's manifests (its members are crates/*; bench/, fuzz/ and
# tests/difftest are workspaces of their own, outside the cache), in path order, each under its
# path. A line that names a `path` is one of the workspace's own crates, which the cache doesn't
# hold; a manifest or a table with nothing else isn't there at all.
manifests=$(git ls-files -- Cargo.toml 'crates/*/Cargo.toml')
config=$(git ls-files -- .cargo/config.toml .cargo/config)
settings=$(
  for f in $manifests; do
    tr -d '\r' < "$f" | awk -v file="$f" '
      function out(line) { if (!named) print "== " file; named = 1; print line }
      /^[ \t]*\[/ {
        h = $0; sub(/^[ \t]*/, "", h); sub(/[ \t]*(#.*)?$/, "", h)
        keep = h ~ /^\[profile[.\]]/ || h == "[features]" ||
          h ~ /^\[(workspace\.|target\..*\.)?(dev-|build-)?dependencies[.\]]/
        head = h
        next
      }
      keep && !/^[ \t]*(#|$)/ && !/(^[ \t]*|[{,][ \t]*)path[ \t]*=/ {
        if (head != "") { out(head); head = "" }
        sub(/[ \t]+$/, ""); out($0)
      }
    '
  done
  for f in $config; do
    echo "== $f"
    tr -d '\r' < "$f" | grep -v '^[[:space:]]*\(#\|$\)' || true
  done
)
build=$(printf '%s\n' "$settings" | git hash-object --stdin | cut -c1-8)
echo "gate-$toolchain-$deps-$build"
