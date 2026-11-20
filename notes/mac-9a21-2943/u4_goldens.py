"""u4_goldens.py: in branches/d1-open-pr (checked out at the U4 draft, velt built), runs `velt check`
on every errors/ golden with a .err: whole (VELT_UNITS unset), VELT_UNITS=2 with std's unit on disk
(a fresh dir, so the first saves and the rest load), and prints the goldens whose error lines
differ, plus how many loaded std's unit from disk. Mode 2 already differs from the default on
about 129 of these (the reviewer's count on #3294); what matters is that loading from disk adds
none: compared against VELT_UNITS=2 without the disk too."""
import os, pathlib, subprocess, tempfile, sys

W = pathlib.Path("/Users/andii/dev/velt/branches/d1-open-pr")
velt = W / "target/debug/velt"
goldens = sorted(p for p in (W / "tests/golden").rglob("*.velt")
                 if "errors" in p.parts and p.with_suffix(".err").exists())
only = sys.argv[1:]
if only:
    goldens = [g for g in goldens if any(o in str(g) for o in only)]
disk = tempfile.mkdtemp(prefix="u4-disk-")
base = {k: v for k, v in os.environ.items() if not k.startswith("VELT_UNITS")}
base["VELT_STD"] = str(W / "std")


def run(g, extra):
    env = dict(base, **extra)
    r = subprocess.run([str(velt), "check", g.name], cwd=g.parent, env=env,
                       capture_output=True, text=True, timeout=300)
    lines = [l for l in r.stderr.splitlines() if "error" in l and not l.startswith("velt-units")]
    return r.returncode, lines, r.stderr


loaded = refused = 0
diff_disk = []
diff_whole = []
for g in goldens:
    whole = run(g, {})
    two = run(g, {"VELT_UNITS": "2"})
    if whole[:2] != two[:2]:
        diff_whole.append(str(g.relative_to(W / "tests/golden")))
    on_disk = run(g, {"VELT_UNITS": "2", "VELT_UNITS_DISK": disk})
    loaded += "unit loaded from disk" in on_disk[2]
    refused += "unit refused from disk" in on_disk[2]
    if two[:2] != on_disk[:2]:
        diff_disk.append(str(g.relative_to(W / "tests/golden")))
print(f"{len(goldens)} goldens; std's unit loaded from disk by {loaded}, refused by {refused}")
print(f"differ between VELT_UNITS=2 and VELT_UNITS=2 from disk: {len(diff_disk)}")
for d in diff_disk:
    print("  ", d)
print(f"VELT_UNITS=2 differs from a whole check on {len(diff_whole)}")
(pathlib.Path(__file__).parent / "u4" / "diff-whole.txt").write_text("\n".join(diff_whole))
sys.exit(1 if diff_disk else 0)
