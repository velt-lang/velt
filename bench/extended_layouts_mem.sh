#!/usr/bin/env bash
# Peak memory of a 1,048,575-node VNode-like tree (#1383): no extension, an extension with five
# optional fields declared only, and the same extension cast to once.
# usage: bench/extended_layouts_mem.sh <velt> <workdir>
set -eu
velt="$1"; work="$2"; mkdir -p "$work"
head='type VNode = { type: string; children: VNode[]; dom: string | null; key: number };'
ext='interface InternalVNode extends VNode {
  _ns?: boolean;
  _subTree?: VNode | null;
  _ref?: { current: VNode | null };
  _props?: Map<string, number>;
  _effect?: () => void;
}'
body='function build(depth: number, key: number): VNode {
  const children: VNode[] = [];
  if (depth > 0) {
    children.push(build(depth - 1, key * 2));
    children.push(build(depth - 1, key * 2 + 1));
  }
  return { type: "div", children, dom: null, key };
}
function count(v: VNode): number {
  let n = 1;
  for (const c of v.children) n += count(c);
  return n;
}
const root = build(19, 1);'
cast='(root as InternalVNode)._ns = true;'
tail='console.log(count(root));'
printf '%s\n%s\n%s\n' "$head" "$body" "$tail" > "$work/none.vlt"
printf '%s\n%s\n%s\n%s\n' "$head" "$ext" "$body" "$tail" > "$work/declared.vlt"
printf '%s\n%s\n%s\n%s\n%s\n' "$head" "$ext" "$body" "$cast" "$tail" > "$work/cast.vlt"
for v in none declared cast; do
  "$velt" build --release "$work/$v.vlt" -o "$work/$v" > /dev/null
  for i in 1 2 3; do
    python3 -c 'import resource,subprocess,sys
subprocess.run([sys.argv[1]],stdout=subprocess.DEVNULL,check=True)
print(sys.argv[2], round(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss/1024,1), "MB peak")' "$work/$v" "$v"
  done
done
