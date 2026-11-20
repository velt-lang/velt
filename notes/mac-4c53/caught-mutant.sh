#!/bin/bash
# The caught_past_limit program alone, against a copy of std with arrayTooLong's panic put back
# (expected: caught, prints "after", exit 0) and against the worktree's std (expected: 101).
W=/Users/andii/dev/velt/branches/alloc-step-a
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/4c53484a-2d67-40a0-a205-9ca8e43ee06d/scratchpad
D=$S/caught-run
rm -rf $D && mkdir -p $D && cd $D || exit 1
V=$W/target/debug/velt
# The probe module, as heap_hooks.rs writes it (from its PROBE constant).
python3 - "$W/crates/veltc/tests/heap_hooks.rs" > probe.velt <<'EOF'
import sys
src = open(sys.argv[1]).read()
start = src.index('const PROBE: &str = r#"') + len('const PROBE: &str = r#"')
print(src[start:src.index('"#;', start)], end="")
EOF
printf 'import { alloc, caught, free, realloc } from "velt/zz_probe";\nfunction main() {\n  console.log("before");\n  caught(() => {\n    realloc(8, 0, 8, 4503599627370496);\n  });\n  console.log("after");\n}\n' > main.velt
for side in mutant fixed; do
  rm -rf std && cp -R $W/std std && mkdir -p std/zz_probe && cp probe.velt std/zz_probe/index.velt
  if [ $side = mutant ]; then
    python3 $S/replace_once.py std/lang/hooks/heap.velt '  velt_rt_flush();
  velt_rt_write_str(
    2,
    "panic: RangeError: Invalid array length: an array holds at most 2^52 - 1 elements, and its elements at most 2^52 - 1 bytes\n",
  );
  velt_rt_exit(101);' '  panic(
    "RangeError: Invalid array length: an array holds at most 2^52 - 1 elements, and its elements at most 2^52 - 1 bytes",
  );' || echo "PATTERN NOT FOUND"
  fi
  VELT_STD=$D/std VELT_HEAP_HOOKS=1 VELT_BUILD_CACHE=off $V run --release main.velt > out.txt 2> err.txt
  echo "$side: exit $?, stdout $(tr '\n' '|' < out.txt), stderr: $(grep -v warning err.txt | head -2 | tr '\n' '|' | cut -c1-200)"
done
cd $S && rm -rf $D
