#!/bin/bash
# #130: does `cc -B<dir> -fuse-ld=lld` take `<dir>/ld.lld` (gcc and clang)? The stand-in logs and
# runs the versioned lld, as a symlink to /usr/bin/ld.lld-18 would.
set -u
d=$(mktemp -d)
cat >"$d/ld.lld" <<EOF
#!/bin/bash
echo used >>"$d/used"
exec /usr/bin/ld.lld-18 "\$@"
EOF
chmod +x "$d/ld.lld"
printf 'int main(void) { return 0; }\n' >"$d/a.c"
for drv in gcc clang; do
  rm -f "$d/used"
  $drv -c "$d/a.c" -o "$d/a.o" && $drv -B"$d" -fuse-ld=lld "$d/a.o" -o "$d/a.out" && "$d/a.out"
  echo "$drv: exit $?, stand-in used: $([ -f "$d/used" ] && echo yes || echo no)"
done
# And a symlink, as velt would make it.
rm -f "$d/ld.lld" "$d/used"; ln -s /usr/bin/ld.lld-18 "$d/ld.lld"
gcc -B"$d" -fuse-ld=lld "$d/a.o" -o "$d/b.out" -Wl,--version 2>&1 | head -1
rm -rf "$d"
