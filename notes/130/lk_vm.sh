#!/bin/bash
# #130 in the OrbStack VM (Linux arm64): what a velt build's link costs with GNU ld, lld and mold,
# for the two links the goldens do with their debug compiler: a debug program against the shared
# debug runtime, and a --release program against the static debug runtime.
#   orb -m velt-prof bash notes/130/lk_vm.sh
# Builds what the test harness builds (debug velt, static and shared debug runtime) from the
# dwarf-overlap worktree (main) into $HOME/b130, captures the cc command velt runs for each
# program, replays it per linker (wall: min of 15; instructions: cachegrind is too slow for a
# linker, so `perf stat` where allowed, else wall only), and deletes $HOME/b130.
set -u
source "$HOME/.cargo/env"
WT=/Users/andii/dev/velt/branches/dwarf-overlap
N=/Users/andii/dev/velt/branches/notes-3097/notes/130
OUT=$HOME/b130
mkdir -p "$OUT/w"
cd "$WT" || exit 1
export CARGO_TARGET_DIR=$OUT/tgt CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
nice -n 19 cargo build -q -p veltc --bin velt || { echo "velt build failed"; exit 1; }
nice -n 19 cargo build -q -p velt_rt || { echo "rt build failed"; exit 1; }
nice -n 19 cargo build -q -p velt_rt_shared || { echo "shared rt build failed"; exit 1; }
ls -la "$OUT"/tgt/debug/libvelt_rt* 2>/dev/null | awk '{print $5, $9}'
which mold >/dev/null 2>&1 || sudo -n apt-get install -y -q mold >/dev/null 2>&1 || echo "note: no mold (apt install failed)"
V=$OUT/tgt/debug/velt
cat >"$OUT/w/ccwrap.sh" <<'EOF'
#!/bin/bash
printf '%s\n' "$@" >"$LINK_ARGS"
exec cc "$@"
EOF
chmod +x "$OUT/w/ccwrap.sh"
timeit() { # label cmd...: min of 15 wall ms
  python3 "$N/minwall.py" "$@"
}
for prog in tests/golden/m1/hello.velt tests/golden/std/jsx_async.velt examples/apps/todo-api; do
  [ -e "$WT/$prog" ] || { echo "skip $prog"; continue; }
  name=$(basename "$prog" .velt)
  for mode in debug release; do
    flag=""; [ $mode = release ] && flag=--release
    export LINK_ARGS=$OUT/w/$name-$mode.args
    rm -f "$LINK_ARGS"
    out=$( cd "$WT" && VELT_LINKER=$OUT/w/ccwrap.sh VELT_STD=$WT/std VELT_BUILD_CACHE=$OUT/cache \
      VELT_BUILD_STAMP=off "$V" build -v $flag "$prog" -o "$OUT/w/$name-$mode" 2>&1 )
    echo "== $name $mode: $(echo "$out" | awk '/velt: link|velt: total/ {printf "%s %s ms  ", $2, $3}')"
    [ -f "$LINK_ARGS" ] || { echo "   no link command captured: $(echo "$out" | tail -2)"; continue; }
    echo "   args: $(tr '\n' ' ' <"$LINK_ARGS" | sed "s#$OUT/##g" | cut -c1-300)"
    mapfile -t args <"$LINK_ARGS"
    for ld in default bfd lld mold; do
      extra=(); [ $ld != default ] && extra=(-fuse-ld=$ld)
      [ $ld = mold ] && ! which mold >/dev/null 2>&1 && continue
      timeit "   cc ${extra[*]:-(default)}" cc "${extra[@]}" "${args[@]}"
    done
  done
done
rm -rf "$OUT"
sudo -n fstrim -a 2>/dev/null
df -h "$HOME" | tail -1
