"""The reviewer's breaks for #3294 (B1, B2), for mutants.py --list; run against veltc's units
tests."""
L = "crates/velt_sema/src/lib.rs"
TEST = ["cargo", "test", "-p", "veltc", "--test", "units"]
MUTANTS = [
    ("B1: the app's copy thawed", L,
     '            units::Stage::App => timings::pass("units: copy std", || std_snap.borrow().clone()),',
     '            units::Stage::App => timings::pass("units: copy std", || {\n                std_snap.borrow().clone().map(|mut c| {\n                    c.thaw_defs();\n                    c\n                })\n            }),'),
    ("B2: std never frozen", L, "        std_cx.freeze_defs();", ""),
    ("the app's stage not thawed before the program", L,
     "    if cx.units_stage == units::Stage::App {\n        cx.thaw_defs();\n    }", ""),
]
