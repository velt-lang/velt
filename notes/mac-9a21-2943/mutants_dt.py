"""The deliberate breaks for #3280 (lazy tables), in the format of mutants.py's MUTANTS; run with
mutants.py's loop by importing this list instead (mutants.py --list mutants_dt)."""
D = "crates/velt_sema/src/ctx/def_table.rs"
S = "crates/velt_sema/src/ctx/slots.rs"
C = "crates/velt_sema/src/units/codec.rs"
T = "crates/velt_sema/src/units/codec/def_table.rs"
MUTANTS = [
    ("M1: get without decoding", D, "        (i < self.items.len()).then(|| self.at(i))", "        self.items.get(i)"),
    ("M2: iter_mut without decode_all", D, "        self.decode_all();\n        self.items.all_mut().iter_mut()", "        self.items.all_mut().iter_mut()"),
    ("entry reader ignores its unit's stack", C, "            stack_floor: self.stack_floor,\n", ""),
    ("a damaged entry is not marked", T, "    if !r.done() {\n        src.mark_damaged();\n    }", "    let _ = r.done();"),
    ("empty entries allowed", T, "if end <= last {", "if end < last {"),
    ("entry start from the table's start", D, "            _ => self.ends[i - 1],", "            _ => self.start,"),
]
