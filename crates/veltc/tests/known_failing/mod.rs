//! The goldens known to fail (`golden_known_failing.txt`, next to `golden.rs`): read by
//! `golden.rs`, whose failures for these only fail the run with `VELT_GOLDEN_STRICT=1`.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// An entry of the list that applies to this run.
pub struct Known {
    /// The issue tracking the failure (`#<number>`).
    pub issue: String,
    /// Marked `units`: it fails only in a run by units, and a run by units fails when it passes.
    pub units: bool,
}

/// The goldens known to fail on this OS, in a run by units when `VELT_UNITS=2`, by path from the
/// repository root.
pub fn known_failing() -> HashMap<String, Known> {
    parse_known_failing(
        include_str!("../golden_known_failing.txt"),
        std::env::consts::OS,
        std::env::var_os("VELT_UNITS").is_some_and(|v| v == "2"),
    )
}

/// Prints `line`, and appends it to the file `VELT_GOLDEN_REPORT` names (the nightly's job
/// summary): libtest keeps a passing test's output to itself.
pub fn report(line: &str) {
    println!("{line}");
    let Some(path) = std::env::var_os("VELT_GOLDEN_REPORT") else {
        return;
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path);
    if let Ok(mut file) = file {
        let _ = writeln!(file, "{line}");
    }
}

/// How a known failure failed, in one line: the first line of its first error and the first line
/// of the compiler's or the program's stderr, so a failure that changes (wrong output to a
/// crash) shows.
pub fn how_it_failed(errs: &[String]) -> String {
    let Some(first) = errs.first() else {
        return String::new();
    };
    let head = first.lines().next().unwrap_or_default();
    // Its first error, else its first line that isn't a check by units' report of writes.
    let stderr = first.split_once("--- stderr ---\n").and_then(|(_, s)| {
        let mut lines = s.lines().map(str::trim).filter(|l| !l.is_empty());
        (lines
            .clone()
            .find(|l| l.contains("error") || l.contains("panicked")))
        .or_else(|| lines.find(|l| !l.starts_with("velt-units")))
    });
    let line = match stderr {
        Some(s) => format!("{head} | {}", s.trim()),
        None => head.to_string(),
    };
    line.chars().take(240).collect()
}

/// The entries of known-failing list `text` that apply on `os`, in a run by units when `units`
/// (`VELT_UNITS=2`). Each line not blank and not a `#`-comment is
/// `<path> #<issue> [os=<os>,...] [units]`: an entry marked `units` fails only in a run by units
/// (#2943); anything else is a mistake in the list.
fn parse_known_failing(text: &str, os: &str, units: bool) -> HashMap<String, Known> {
    let mut out = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(path), Some(issue)) = (words.next(), words.next()) else {
            panic!("golden_known_failing.txt: `{line}` needs a path and an issue");
        };
        assert!(
            issue.len() > 1
                && issue[1..].bytes().all(|b| b.is_ascii_digit())
                && issue.starts_with('#'),
            "golden_known_failing.txt: `{line}`: the issue is `#<number>`"
        );
        let (mut on_os, mut by_units) = (true, false);
        for w in words.by_ref() {
            match (w.strip_prefix("os="), w) {
                (Some(list), _) => on_os = list.split(',').any(|o| o == os),
                (None, "units") => by_units = true,
                _ => panic!(
                    "golden_known_failing.txt: `{line}`: after the issue only `os=...` and `units`"
                ),
            }
        }
        if on_os && (!by_units || units) {
            let issue = issue.to_string();
            out.insert(
                path.to_string(),
                Known {
                    issue,
                    units: by_units,
                },
            );
        }
    }
    out
}

#[test]
fn the_known_failing_list_reads_its_entries_by_os() {
    let text = "# comment\n\ntests/golden/a.velt #12\ntests/golden/b.velt #34 os=windows,linux\ntests/golden/c.velt #56 units\ntests/golden/d.velt #78 os=linux units\n";
    let mac = parse_known_failing(text, "macos", false);
    assert_eq!(
        mac.get("tests/golden/a.velt").map(|k| k.issue.as_str()),
        Some("#12")
    );
    assert!(!mac.contains_key("tests/golden/b.velt"));
    assert!(!mac.contains_key("tests/golden/c.velt"));
    let linux = parse_known_failing(text, "linux", false);
    assert_eq!(
        linux.get("tests/golden/b.velt").map(|k| k.issue.as_str()),
        Some("#34")
    );
    assert!(!linux.contains_key("tests/golden/d.velt"));
    // An entry marked `units` applies in a run by units only, on its OSes.
    let mac_units = parse_known_failing(text, "macos", true);
    assert_eq!(
        mac_units
            .get("tests/golden/c.velt")
            .map(|k| k.issue.as_str()),
        Some("#56")
    );
    assert!(!mac_units.contains_key("tests/golden/d.velt"));
    // Only an entry marked `units` fails a run by units when it passes.
    assert!(mac_units["tests/golden/c.velt"].units);
    assert!(!mac_units["tests/golden/a.velt"].units);
    let linux_units = parse_known_failing(text, "linux", true);
    assert!(linux_units.contains_key("tests/golden/d.velt"));
    // The words after the issue in either order.
    let either = parse_known_failing(
        "tests/golden/e.velt #90 units os=windows\n",
        "windows",
        true,
    );
    assert!(either.contains_key("tests/golden/e.velt"));
    let either = parse_known_failing("tests/golden/e.velt #90 units os=windows\n", "linux", true);
    assert!(!either.contains_key("tests/golden/e.velt"));
    // Every entry of the real list names a golden that is there, on any OS.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let all = include_str!("../golden_known_failing.txt");
    for os in ["macos", "linux", "windows"] {
        for path in parse_known_failing(all, os, true).keys() {
            assert!(
                root.join(path).is_file(),
                "golden_known_failing.txt: no golden {path}"
            );
        }
    }
}

#[test]
fn a_known_failure_says_how_it_failed() {
    let ice = "tests/golden/a.velt [debug]: the build failed\n--- stderr ---\n\nvelt-units: writes to frozen definitions\nvelt-units:   2  a.rs:1\nerror: internal compiler error: boom\nmore\n".to_string();
    assert_eq!(
        how_it_failed(&[ice]),
        "tests/golden/a.velt [debug]: the build failed | error: internal compiler error: boom"
    );
    let wrong = "tests/golden/b.velt [release]: exit 0 (want 0)\n--- want ---\n1\n--- got ---\n2\n--- stderr ---\n".to_string();
    assert_eq!(
        how_it_failed(&[wrong]),
        "tests/golden/b.velt [release]: exit 0 (want 0)"
    );
    assert_eq!(how_it_failed(&[]), "");
}
