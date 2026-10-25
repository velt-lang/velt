#!/usr/bin/env bash
# A `$VELT_CLANG` that edits the LLVM IR a release build hands clang (`-x ir`, on stdin) with the
# Python script `$PATCH` before compiling it, for measuring what a change to the IR would do:
#
#   PATCH=bench/array_view/ir/exit_unreachable.py VELT_CLANG=bench/array_view/ir/clang.sh \
#     velt build --release rows_index.vlt
#
# `$CLANG` is the real clang (default: clang on PATH).
for a in "$@"; do
  if [ "$a" = ir ]; then
    python3 "$PATCH" | "${CLANG:-clang}" "$@"
    exit $?
  fi
done
exec "${CLANG:-clang}" "$@"
