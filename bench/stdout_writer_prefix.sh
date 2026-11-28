#!/usr/bin/env bash
# An installed toolchain from a checkout, for bench/stdout_writer_ab.sh (Linux): a release build
# of velt and its runtimes, laid out as a toolchain is (`bin/velt`, `lib/` runtimes, `std/`,
# `lib/hooks/<target>` from `velt cache hooks`), so that a build finds the shipped objects.
#
#   bench/stdout_writer_prefix.sh CHECKOUT PREFIX
#
# Builds into $CARGO_TARGET_DIR (default CHECKOUT/target). Then, per side:
#   bench/stdout_writer_ab.sh BASE/bin/velt BASE/std HEAD/bin/velt HEAD/std
#
# The cold rows include, once per build cache, the adoption of the writer's shipped object
# (q64 exempts it). Its share comes from callgrind on head's cold build:
#   VELT_STD=HEAD/std VELT_BUILD_STAMP=off VELT_BUILD_CACHE=<empty dir> \
#     valgrind --tool=callgrind HEAD/bin/velt build hello.velt -o hello
#   callgrind_annotate --inclusive=yes callgrind.out.* | grep 'prebuilt::Finder>::find'
set -euo pipefail
SRC=$(cd "${1:?checkout}" && pwd)
P=${2:?prefix}
T=${CARGO_TARGET_DIR:-$SRC/target}
(cd "$SRC" && CARGO_TARGET_DIR=$T cargo build -q --release -p veltc -p velt_rt -p velt_rt_shared)
mkdir -p "$P/bin" "$P/lib" "$P/std" "$P/share/velt/warm"
cp "$T/release/velt" "$P/bin/"
cp "$T/release/libvelt_rt.a" "$T/release/libvelt_rt_shared.so" "$P/lib/"
cp -R "$SRC/std/." "$P/std/"
if [[ -d $SRC/share/velt/warm ]]; then
  cp -R "$SRC/share/velt/warm/." "$P/share/velt/warm/"
fi
env -u VELT_STD VELT_BUILD_CACHE=off "$P/bin/velt" cache hooks --out "$P/lib/hooks"
