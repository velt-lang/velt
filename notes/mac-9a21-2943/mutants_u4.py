"""Breaks for U4, for mutants.py --list: each must fail the disk test."""
TEST = ["cargo", "test", "-p", "veltc", "--test", "units", "std_s_unit_on_disk"]
L = "crates/velt_sema/src/lib.rs"
MUTANTS = [
    ("std's unit never saved", L,
     "                if let Some(path) = &disk {\n                    units::disk::save(path, &std_cx);\n                }\n", ""),
    ("the driver's key differs per program", "crates/veltc/src/driver/units_key.rs",
     "    for m in std {\n", "    h.part(sess.sm.get(loaded.modules[loaded.root].file).src.as_bytes());\n    for m in std {\n"),
]
