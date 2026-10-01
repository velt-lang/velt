#!/usr/bin/env bash
# Builds the compiler and the differential tester, then runs `difftest` with the given arguments.
#   tests/difftest/run.sh run tests/difftest/corpus      # check the hand-written corpus
#   tests/difftest/run.sh fuzz --seeds 1..500            # generate, check and shrink
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cargo build --quiet --manifest-path "$here/../../Cargo.toml" -p veltc
cargo build --quiet --release --manifest-path "$here/Cargo.toml"
exec "$here/target/release/difftest" "$@"
