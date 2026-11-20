"""Breaks for U3c, for mutants.py --list: each fix taken out must fail its test."""
TEST = ["cargo", "test", "-p", "veltc", "--test", "units", "std_s_unit_ends_clean"]
MUTANTS = [
    ("std's memos kept", "crates/velt_sema/src/units.rs",
     "    codec::unit::clear_memos(cx);\n", ""),
    ("the app's collection leaves bodies allowed", "crates/velt_sema/src/collect/mod.rs",
     "    if app {\n        cx.bodies_ok = false;\n    }\n", ""),
]
