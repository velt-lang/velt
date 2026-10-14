#!/bin/bash
# Builds everything bench_chat.py runs, on the VM (Ubuntu 24.04 with build-essential, clang,
# Rust, Node 24, Bun and Go installed by up.sh).
set -euo pipefail
export PATH=$HOME/.cargo/bin:$HOME/.bun/bin:/usr/local/go/bin:$PATH
cd ~/velt
export VELT_STD=$PWD/std
cargo build --release -q -p veltc -p velt_rt
mkdir -p bench/chat/.work
target/release/velt build --release bench/chat/velt/server.vlt -o bench/chat/.work/chat-velt
target/release/velt build --release bench/chat/velt/server-topics.vlt -o bench/chat/.work/chat-velt-topics
(cd bench/chat/go && go build -o chat-go .)
(cd bench/chat/node && npm ci --silent)
(cd bench/chat/load && cargo build --release -q)
ls -la bench/chat/.work/chat-velt bench/chat/.work/chat-velt-topics bench/chat/go/chat-go bench/chat/load/target/release/chat-load
