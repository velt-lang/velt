#!/usr/bin/env bash
# TEMPORARY spike (#803): Linux links with ld.lld and no cc. glibc imports + musl static.
set -uo pipefail
OUT=${1:-spike-out}; mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
sec() { printf '\n===== %s =====\n' "$1"; }
arch=$(uname -m)
SYSROOT=$(rustc --print sysroot)
LLD="$SYSROOT/lib/rustlib/$arch-unknown-linux-gnu/bin/rust-lld"
sec "rust-lld"; ls -la "$LLD"; ldd "$LLD"
sec build
cargo build --release -p veltc -p velt_rt 2>&1 | tail -2
velt=target/release/velt; rt=target/release/libvelt_rt.a
$velt build --emit obj tests/golden/m1/hello.vlt -o "$OUT/hello.o"
$velt build --emit obj examples/http_hello.vlt -o "$OUT/http.o"

sec "glibc imports (all_load superset), with versions"
"$LLD" -flavor gnu -o "$OUT/all.so" -shared --whole-archive "$rt" --no-whole-archive \
  --unresolved-symbols=ignore-all 2>&1 | tail -3
cc -o "$OUT/http-cc" "$OUT/http.o" "$rt" -pie -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc
readelf --dyn-syms -W "$OUT/http-cc" | awk '$7=="UND" && $8!="" {print $8}' | sort -u > "$OUT/http-imports.txt"
wc -l < "$OUT/http-imports.txt"; grep -v GLIBC_2.2.5 "$OUT/http-imports.txt" | grep -v GLIBC_2.17 | head -40
readelf -d "$OUT/http-cc" | grep NEEDED
nm -u "$OUT/all.so" | awk '{print $2}' | sort -u > "$OUT/all-undef.txt"; wc -l < "$OUT/all-undef.txt"

sec "glibc: link with ld.lld against the system libs (no cc)"
gnu=/usr/lib/$arch-linux-gnu
case $arch in x86_64) dl=/lib64/ld-linux-x86-64.so.2 ;; *) dl=/lib/ld-linux-aarch64.so.1 ;; esac
"$LLD" -flavor gnu -pie --dynamic-linker "$dl" -o "$OUT/http-lld" \
  $gnu/Scrt1.o $gnu/crti.o "$OUT/http.o" "$rt" -L$gnu -L/lib/$arch-linux-gnu --as-needed \
  -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc $gnu/crtn.o 2>&1 | tail -5
VELT_HELLO_SECONDS=1 "$OUT/http-lld"; echo "exit $?"

sec "musl static"
rustup target add $arch-unknown-linux-musl >/dev/null 2>&1
sudo apt-get install -y -qq musl-tools >/dev/null 2>&1
cargo build --release -p velt_rt --target $arch-unknown-linux-musl 2>&1 | tail -2
mrt=target/$arch-unknown-linux-musl/release/libvelt_rt.a; ls -la $mrt
SC="$SYSROOT/lib/rustlib/$arch-unknown-linux-musl/lib/self-contained"; ls "$SC"
$velt build --emit obj --target $arch-unknown-linux-musl tests/golden/m1/hello.vlt -o "$OUT/mhello.o" 2>&1 | tail -3
[ -f "$OUT/mhello.o" ] || cp "$OUT/hello.o" "$OUT/mhello.o"
cp "$OUT/http.o" "$OUT/mhttp.o"
for p in mhello mhttp; do
  "$LLD" -flavor gnu -static -no-pie -o "$OUT/$p" "$SC/crt1.o" "$SC/crti.o" "$SC/crtbegin.o" \
    "$OUT/$p.o" "$mrt" "$SC/libunwind.a" "$SC/libc.a" "$SC/crtend.o" "$SC/crtn.o" 2>&1 | tail -15
  file "$OUT/$p" 2>/dev/null; ls -la "$OUT/$p"
done
"$OUT/mhello"; echo "exit $?"
(VELT_HELLO_SECONDS=2 "$OUT/mhttp" &) ; sleep 1; curl -s localhost:8080/json; echo; sleep 2
sec "musl binaries in debian slim (no build-essential)"
docker run --rm -v "$OUT:/o" debian:bookworm-slim sh -c '/o/mhello; echo exit $?; ls /usr/bin/cc /usr/bin/ld 2>&1'
