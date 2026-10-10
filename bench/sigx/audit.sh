#!/usr/bin/env bash
# Counts the diagnostics `velt check` reports on sigx's packages (the audit of #823), deduplicated
# by location, per package, and those about `symbol` (a message or source line mentioning it).
#
#   bench/sigx/audit.sh <velt binary> <sigx checkout with the audit package.vlt> [out.json]
#
# The checkout needs a `package.vlt` at its root whose `paths` alias the sigx packages (the
# audit's setup). Nothing in the checkout is run; it is only read by `velt check`.
set -euo pipefail
velt=$1
root=$2
out=${3:-/dev/null}
cd "$root"
tmp=$(mktemp -d)
for pkg in reactivity runtime-core server-renderer serialize sigx runtime-dom; do
  find "packages/$pkg/src" \( -name '*.ts' -o -name '*.tsx' \) ! -name '*.d.ts' | sort |
    while read -r f; do
      "$velt" check "$f" --json 2>/dev/null >"$tmp/one.json" || true
      python -I - "$tmp/one.json" "$pkg" >>"$tmp/all.tsv" <<'PY'
import json, sys
try:
    doc = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    sys.exit(0)
for d in doc.get("diagnostics", []):
    if d.get("severity") != "error":
        continue
    loc = d.get("location") or {}
    f = loc.get("file", "").replace("\\", "/")
    i = f.find("packages/")
    f = f[i:] if i >= 0 else f
    msg = d.get("message", "").replace("\t", " ").replace("\n", " ")
    print(f"{f}\t{loc.get('line')}\t{loc.get('column')}\t{msg}")
PY
    done
done
python -I - "$tmp/all.tsv" "$out" <<'PY'
import json, re, sys
seen = {}
for line in open(sys.argv[1], encoding="utf-8"):
    f, l, c, msg = line.rstrip("\n").split("\t", 3)
    seen[(f, l, c)] = msg
per = {}
sym = 0
sym_rows = []
for (f, l, c), msg in sorted(seen.items()):
    m = re.match(r"packages/([^/]+)/", f)
    pkg = m.group(1) if m else "other"
    per[pkg] = per.get(pkg, 0) + 1
    try:
        src = open(f, encoding="utf-8").read().splitlines()[int(l) - 1]
    except Exception:
        src = ""
    if re.search(r"symbol|Symbol", msg + " " + src):
        sym += 1
        sym_rows.append(f"{f}:{l}:{c}: {msg}")
print("total", len(seen))
for k in sorted(per):
    print(k, per[k])
print("symbol-related", sym)
for r in sym_rows:
    print("  ", r)
json.dump({"total": len(seen), "per": per, "symbol": sym, "rows": sym_rows}, open(sys.argv[2], "w"))
PY
rm -rf "$tmp"
