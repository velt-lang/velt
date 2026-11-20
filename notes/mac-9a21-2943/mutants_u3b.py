"""Breaks for U3b (std's summaries of record keys, JSON and struct tuples), for mutants.py --list;
each summary not kept by std's stage, run against veltc's units tests."""
TEST = ["cargo", "test", "-p", "veltc", "--test", "units", "std_s_summary"]
MUTANTS = [
    ("JSON's summary not kept", "crates/velt_sema/src/json/mod.rs",
     "        cx.json_needs = JsonNeeds(needs);", "        let _ = &needs;"),
]
