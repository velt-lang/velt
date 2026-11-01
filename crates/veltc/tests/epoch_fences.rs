//! `velt/sync/epoch`'s ordering (docs/internals/design/atomics.md "Reclamation"): `pin` stores
//! its announcement, then a `seqcst` fence; `retire` takes a `seqcst` fence before it reads the
//! epoch; `collect`'s advance takes one before it scans the slots. A missing fence lets a value
//! be dropped while a reader on a weakly ordered CPU can still reach it, which no test run can
//! be counted on to show, so the fences are checked in the optimized LLVM IR.

mod no_window;
mod test_dir;

/// The `fence seq_cst` instructions in the release LLVM IR of `src`, compiled as `main.vlt`.
fn seqcst_fences(src: &str) -> usize {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--release", "--emit", "llvm"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .matches("fence seq_cst")
        .count()
}

#[test]
fn pin_has_its_fence() {
    let src = "import { pin, unpin } from \"velt/sync/epoch\";\nfunction main() {\n  pin();\n  unpin();\n}\n";
    assert_eq!(seqcst_fences(src), 1);
}

#[test]
fn retire_and_advance_have_theirs() {
    // `retire`'s own fence, and the one in `collect`'s advance (`retire` collects every 64).
    // At least: other inlined copies of `retire` (the module's own stores of reference words,
    // once the compiler retires what they replace) add theirs; without `retire`'s fence only
    // the advance's is left.
    let src = "import { retire } from \"velt/sync/epoch\";\ninterface B {\n  n: u64;\n}\nfunction main() {\n  retire(shared({ n: 1 }));\n}\n";
    let n = seqcst_fences(src);
    assert!(n >= 2, "{n} seqcst fences");
}
