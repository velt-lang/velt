//! `VELT_STD_SUMMARY` (the std identity oracle, #2943, `driver/std_summary.rs`): the whole-std mode
//! `VELT_STD_SUMMARY_EAGER=1` checks std definitions the program doesn't reach, and a definition's
//! hashed form names the C exports it takes the address of instead of numbering them.

mod no_window;
mod test_dir;

use std::collections::BTreeSet;
use std::path::Path;

/// Checks `src` as `main.velt` in `dir` with the summary written to `dir/summary` (and the hashed
/// forms of the definitions whose names contain `show`, when given), and `VELT_STD_SUMMARY_EAGER`
/// set when `eager`.
fn check(dir: &Path, src: &str, eager: bool, show: Option<&str>) {
    std::fs::write(dir.join("main.velt"), src).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["check", "main.velt"])
        .current_dir(dir)
        .env("VELT_STD_SUMMARY", dir.join("summary"))
        .env_remove("VELT_STD_SUMMARY_EAGER")
        .env_remove("VELT_STD_SUMMARY_SHOW");
    if eager {
        cmd.env("VELT_STD_SUMMARY_EAGER", "1");
    }
    if let Some(part) = show {
        cmd.env("VELT_STD_SUMMARY_SHOW", part);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The std definitions the summary in `dir` names.
fn names(dir: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(dir.join("summary")).unwrap();
    (text.lines())
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split('\t').next())
        .map(str::to_string)
        .collect()
}

const HELLO: &str = "function main() { console.log(\"hi\"); }";

#[test]
fn the_whole_std_mode_checks_what_the_program_does_not_reach() {
    let (lazy, eager) = (test_dir::TestDir::new(), test_dir::TestDir::new());
    check(lazy.path(), HELLO, false, None);
    check(eager.path(), HELLO, true, None);
    let (lazy, eager) = (names(lazy.path()), names(eager.path()));
    // JSON's parser: std code `hello` never calls.
    let unreached = |n: &String| n.starts_with("std/prelude/json/");
    assert!(!lazy.iter().any(unreached), "a lazy check reached JSON");
    assert!(
        eager.iter().any(unreached),
        "the whole-std check left JSON out"
    );
    assert!(
        lazy.len() < eager.len(),
        "{} vs {}",
        lazy.len(),
        eager.len()
    );
}

#[test]
fn a_c_export_is_named_not_numbered() {
    let dir = test_dir::TestDir::new();
    let src = "async function main() { await new Promise<void>((r) => setTimeout(r, 1)); console.log(\"hi\"); }";
    check(dir.path(), src, false, Some("std/lang/hooks/sched"));
    // `exportedAddress(f)`: an index into the program's C exports, shown as the export's name.
    const EXPORTED: &str = "callee: Intrinsic(ExportedAddress), args: [Expr { kind: Lit(Int(";
    let mut named = 0;
    for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("summary.") {
            continue;
        }
        // The shown form has a field per line.
        let text = std::fs::read_to_string(entry.path()).unwrap();
        let text = text.replace(",\n", ", ");
        for (at, m) in text.match_indices(EXPORTED) {
            let arg = &text[at + m.len()..];
            assert!(
                !arg.starts_with(|c: char| c.is_ascii_digit()),
                "{name} numbers a C export: {}",
                &arg[..arg.len().min(40)]
            );
            named += 1;
        }
    }
    assert!(
        named > 0,
        "no std scheduler hook takes a C export's address"
    );
}
