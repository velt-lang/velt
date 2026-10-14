#!/bin/bash
# Builds everything the VM benchmark runs (bench_vm.py).
set -euo pipefail
export PATH=$HOME/.cargo/bin:$HOME/.bun/bin:$PATH
cd ~/velt
export VELT_STD=$PWD/std
W=bench/websocket/.work
mkdir -p $W
cargo build --release -q -p veltc -p velt_rt
V=target/release/velt
$V build --release bench/websocket/velt-raw/server.vlt -o $W/velt-raw-server
$V build --release bench/websocket/velt/server.vlt -o $W/velt-server
(cd bench/websocket/load && cargo build --release -q)
(cd bench/websocket/rust && cargo build --release -q)
(cd bench/websocket/node && npm ci --silent)
ls -la $W bench/websocket/load/target/release/ws-load bench/websocket/rust/target/release/ws-bench-rust
