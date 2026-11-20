"""mutants.py: in branches/d1-open-pr (checked out at the commit under test), applies each
deliberate break in turn (one text replacement), runs the unit-codec tests of velt_sema (and
velt_syntax's codec tests for a break there), restores the file, and prints whether the tests
caught it. Exit status 1 if a break survives or does not apply."""
import pathlib, subprocess, sys, os

W = pathlib.Path("/Users/andii/dev/velt/branches/d1-open-pr")
V = "crates/velt_sema/src/units/codec/values.rs"
C = "crates/velt_sema/src/units/codec.rs"
A = "crates/velt_sema/src/units/codec/ast_refs.rs"
SY = "crates/velt_syntax/src/codec/mod.rs"
MUTANTS = [
    ("bool decode swapped", V, "            0 => false,\n            1 => true,", "            0 => true,\n            1 => false,"),
    ("i128 zigzag broken", V, "((v >> 1) as i128) ^ -((v & 1) as i128)", "(v >> 1) as i128"),
    ("u128 top-bits check loosened", V, "if shift == 126 && b > 3 {", "if shift == 126 && b > 127 {"),
    ("u64 overflow check moved", C, "if shift == 63 && b > 1 {", "if shift == 70 && b > 1 {"),
    ("map sort removed", V, "        let v = sorted_by_key(self.iter());\n        w.uint(v.len() as u64);\n        for (k, val) in v {",
     "        let v: Vec<_> = self.iter().collect();\n        w.uint(v.len() as u64);\n        for (k, val) in v {"),
    ("BTreeMap key and value swapped on decode", V,
     "        let mut out = std::collections::BTreeMap::new();\n        for _ in 0..n {\n            if r.failed() {\n                break;\n            }\n            let k = K::dec(r);\n            out.insert(k, V::dec(r));",
     "        let mut out = std::collections::BTreeMap::new();\n        for _ in 0..n {\n            if r.failed() {\n                break;\n            }\n            let v = V::dec(r);\n            out.insert(K::dec(r), v);"),
    ("Reader::len's bound removed", C, "        if n > (self.bytes.len() - self.pos) as u64 {\n            self.fail();\n            return 0;\n        }\n        n as usize",
     "        n as usize"),
    ("unknown tag no longer fails the reader", C, "                    _ => {\n                        r.fail_damaged();\n                        Self::dec(r)", "                    _ => {\n                        Self::dec(r)"),
    ("Frags::node's type check removed", C, "return (t == id).then_some(p.cast());", "return Some(p.cast());"),
    ("class method bodies kept", A, "            m.decl.body = no_body(&m.decl.body);", "            let _ = &m;"),
    ("interface method bodies kept", A, "                *b = no_body(b);", "                let _ = &b;"),
    ("owned TypeExpr's file decoded wrong", A, "    fn dec(r: &mut Reader) -> Self {\n        let file = r.file();\n        let n = r.len();", "    fn dec(r: &mut Reader) -> Self {\n        let file = FileId(r.file().0 + 1);\n        let n = r.len();"),
    ("header not checked", C, "        if self.take(want.len()) != want.as_slice() {\n            self.fail();\n        }", "        self.take(want.len());"),
    ("reversed span saturates", V, "self.hi.wrapping_sub(self.lo)", "self.hi.saturating_sub(self.lo)"),
    ("foreign file stored raw", SY, "                Some(number) => number(self.file),", "                Some(_) => self.file.0,"),
    ("decode_fragment accepts trailing bytes", SY, "        let node = T::dec_fragment(&mut r);\n        (!r.failed && r.pos == r.bytes.len()).then_some(node)", "        let node = T::dec_fragment(&mut r);\n        (!r.failed).then_some(node)"),
    ("Src::reader slices unchecked", C, "        let (bytes, inside) = match self.bytes.get(range) {\n            Some(b) => (b, true),\n            None => (&self.bytes[..0], false),\n        };", "        let (bytes, inside) = (&self.bytes[range], true);"),
]
env = dict(os.environ, CARGO_BUILD_JOBS="3", CARGO_INCREMENTAL="0", SDKROOT="/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk")
for k in ("VELT_STD", "VELT_UNITS", "VELT_BUILD_STAMP"):
    env.pop(k, None)
only = sys.argv[1:]
# `--list <module>`: the breaks of another list (a module beside this file with MUTANTS).
if only[:1] == ["--list"]:
    sys.path.insert(0, str(pathlib.Path(__file__).parent))
    listed = __import__(only[1])
    MUTANTS = listed.MUTANTS
    # The list's own test command, if it names one.
    TEST = getattr(listed, "TEST", None)
    only = only[2:]
else:
    TEST = None
bad = 0
for name, rel, old, new in MUTANTS:
    if only and not any(o in name for o in only):
        continue
    p = W / rel
    text = p.read_text()
    # A break is one replacement, or a list of them (old and new as lists, applied in order).
    pairs = list(zip(old, new)) if isinstance(old, list) else [(old, new)]
    broken = text
    missing = [o for o, _ in pairs if broken.count(o) != 1]
    if missing:
        print("DOES NOT APPLY: %s" % name, flush=True)
        bad += 1
        continue
    for o, n in pairs:
        broken = broken.replace(o, n)
    p.write_text(broken)
    try:
        if TEST:
            cmd = TEST
        elif rel == SY:
            cmd = ["cargo", "test", "-p", "velt_syntax", "--lib", "codec", "-p", "velt_sema", "--", "codec"]
            cmd = ["cargo", "test", "-p", "velt_syntax", "-p", "velt_sema", "--lib", "codec"]
        else:
            cmd = ["cargo", "test", "-p", "velt_sema", "--lib", "--", "units::codec"]
        r = subprocess.run(cmd, cwd=W, env=env, capture_output=True, text=True)
    finally:
        p.write_text(text)
    out = r.stdout + r.stderr
    failed = [l.split()[1] for l in out.splitlines() if l.startswith("test ") and l.endswith("FAILED")]
    if r.returncode == 0:
        print("SURVIVED: %s" % name, flush=True)
        bad += 1
    elif failed:
        print("caught: %s  (%s)" % (name, ", ".join(sorted(set(f.split("::")[-1] for f in failed))[:3])), flush=True)
    else:
        tail = [l for l in out.splitlines() if "error" in l or "overflow" in l or "panicked" in l][:2]
        print("caught without a failing test line: %s  (%s)" % (name, " | ".join(t[:120] for t in tail)), flush=True)
sys.exit(1 if bad else 0)
