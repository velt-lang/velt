#!/usr/bin/env python3
"""Counts the errors `velt check` reports on the sigx packages: how much of a real TypeScript
code base Velt accepts as is (standard library only, Python 3.8+).

  count_errors.py <velt> <sigx checkout> <out.json>

<sigx checkout> is a clone of https://github.com/signalxjs/core. The script checks every
`.ts` / `.tsx` file (not `.d.ts`) under `packages/<p>/src` for the packages below, one
`velt check --json` per file, and counts distinct errors per package, deduplicated by file,
line and column (a module's errors are reported again by every file importing it). It prints
the counts and writes every error to <out.json>, so two runs can be compared with a diff.

The packages import one another by their npm names; the checkout needs a `package.vlt` that
maps them to sources. The script writes the one below when the checkout has none.
"""

import json
import os
import subprocess
import sys
from collections import Counter

PACKAGES = ["reactivity", "runtime-core", "server-renderer", "serialize", "sigx", "runtime-dom"]

PACKAGE_VLT = """import type { Package } from "velt:package";

export const pkg: Package = {
  name: "sigx-audit",
  version: "0.0.0",
  paths: {
    "@sigx/reactivity": "packages/reactivity/src/index",
    "@sigx/reactivity/internals": "packages/reactivity/src/internals",
    "@sigx/runtime-core": "packages/runtime-core/src/index",
    "@sigx/runtime-core/internals": "packages/runtime-core/src/internals",
    "@sigx/runtime-dom": "packages/runtime-dom/src/index",
    "@sigx/runtime-dom/internals": "packages/runtime-dom/src/internals",
    "@sigx/runtime-dom/platform": "packages/runtime-dom/src/platform",
    "@sigx/serialize": "packages/serialize/src/index",
    "@sigx/serialize/stringify": "packages/serialize/src/stringify",
    "sigx": "packages/sigx/src/index",
    "sigx/internals": "packages/sigx/src/internals",
  },
};
"""


def package_of(path):
    for p in PACKAGES:
        if f"packages/{p}/" in path:
            return p
    return "other"


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    velt, root, out = sys.argv[1], os.path.abspath(sys.argv[2]), sys.argv[3]
    manifest = os.path.join(root, "package.vlt")
    if not os.path.exists(manifest):
        with open(manifest, "w", encoding="utf-8") as f:
            f.write(PACKAGE_VLT)
    seen = {}
    for p in PACKAGES:
        src = os.path.join(root, "packages", p, "src")
        for d, _, files in os.walk(src):
            for name in sorted(files):
                if not name.endswith((".ts", ".tsx")) or name.endswith(".d.ts"):
                    continue
                rel = os.path.relpath(os.path.join(d, name), root).replace("\\", "/")
                r = subprocess.run(
                    [velt, "check", rel, "--json"],
                    cwd=root,
                    capture_output=True,
                    text=True,
                    encoding="utf-8",
                )
                try:
                    report = json.loads(r.stdout)
                except ValueError:
                    print("no JSON report for", rel, r.stdout[:200], r.stderr[:200])
                    continue
                for dg in report.get("diagnostics", []):
                    if dg.get("severity") != "error":
                        continue
                    loc = dg.get("location") or {}
                    f = os.path.normcase(os.path.abspath(os.path.join(root, loc.get("file") or rel)))
                    f = os.path.relpath(f, os.path.normcase(root)).replace("\\", "/")
                    seen[(f, loc.get("line"), loc.get("column"))] = dg["message"]
    counts = Counter(package_of(k[0]) for k in seen)
    for p in PACKAGES + ["other"]:
        print(f"{p:16} {counts.get(p, 0)}")
    print(f"{'total':16} {len(seen)}")
    rows = [
        {"file": k[0], "line": k[1], "col": k[2], "msg": m}
        for k, m in sorted(seen.items(), key=lambda kv: str(kv[0]))
    ]
    with open(out, "w", encoding="utf-8") as f:
        json.dump(rows, f, indent=0)


if __name__ == "__main__":
    main()
