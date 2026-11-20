#!/bin/bash
# #130 in the VM: velt_link's tests and clippy on Linux (the versioned-lld link test), then the
# same test with the versioned lookup taken out (it must fail). Ends by deleting its build dir.
#   orb -m velt-prof bash notes/130/vm_link_test.sh
set -u
source "$HOME/.cargo/env"
WT=/Users/andii/dev/velt/branches/link-versioned-lld
OUT=$HOME/b130t
export CARGO_TARGET_DIR=$OUT CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
cd "$WT" || exit 1
cargo clippy -q -p velt_link --all-targets -- -D warnings 2>&1 | tail -15; echo "clippy exit ${PIPESTATUS[0]}"
cargo test -q -p velt_link 2>&1 | grep -E "^test .*FAILED|test result|panicked|note:" | head -12
cargo test -p velt_link --test link_host a_versioned_lld 2>&1 | grep -E "^test |note: no lld" | head -4
# Mutant: the search stops before the versioned names (a copy of the tree is not needed: the
# file is restored right after).
F=crates/velt_link/src/fast_ld.rs
cp "$F" "$OUT/fast_ld.keep"
perl -0pi -e 's/    LLD_VERSIONS\.rev\(\)\.find_map\(\|v\| \{/    (0..0u32).find_map(|v| {/' "$F"
grep -c "(0..0u32).find_map" "$F"
cargo test -p velt_link --test link_host a_versioned_lld 2>&1 | grep -E "^test |did not run|link failed|panicked" | cut -c1-200 | head -8
cp "$OUT/fast_ld.keep" "$F"
cd "$WT" && git status --short | head -5
rm -rf "$OUT"
df -h "$HOME" | tail -1
