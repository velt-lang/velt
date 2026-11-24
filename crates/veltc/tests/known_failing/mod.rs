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

/// What a run does with a golden that passed.
#[derive(Debug, PartialEq)]
pub enum OnPass {
    /// Fail the run with this error: an entry marked `units`, which applies only in a run by
    /// units, so the list of what units still get wrong shrinks (#2943).
    Fail(String),
    /// Report this line: another entry, to be taken off the list.
    Report(String),
    /// An unlisted golden.
    Nothing,
}

/// What a run does with golden `rel`, which passed, listed as `entry`.
pub fn on_pass(rel: &str, entry: Option<&Known>) -> OnPass {
    let Some(k) = entry else {
        return OnPass::Nothing;
    };
    let line = format!(
        "{rel} passes: take it off golden_known_failing.txt ({})",
        k.issue
    );
    match k.units {
        true => OnPass::Fail(line),
        false => OnPass::Report(format!("golden: {line}")),
    }
}

/// How a known failure failed, in one line: the first line of its first error with output (a
/// leak check's missing report comes first when the build failed, and says nothing of why), the
/// first line where the output differs, and the first error line of the compiler's or the
/// program's stderr, so a failure that changes (wrong output to a crash) shows.
pub fn how_it_failed(errs: &[String]) -> String {
    let with_output = |e: &&String| e.contains("--- stderr ---\n") || e.contains("--- got ---\n");
    let Some(err) = errs.iter().find(with_output).or(errs.first()) else {
        return String::new();
    };
    let head = err.lines().next().unwrap_or_default().to_string();
    let parts: Vec<String> = std::iter::once(head)
        .chain(first_difference(err))
        .chain(stderr_line(err).map(str::to_string))
        .collect();
    parts.join(" | ").chars().take(400).collect()
}

/// Where error `err`'s output first differs from the wanted one: "want `a`, got `b`".
fn first_difference(err: &str) -> Option<String> {
    let (_, rest) = err.split_once("--- want ---\n")?;
    let (want, rest) = rest.split_once("--- got ---\n")?;
    let got = rest.split_once("--- stderr ---\n").map_or(rest, |(g, _)| g);
    let (mut w, mut g) = (want.lines(), got.lines());
    loop {
        match (w.next(), g.next()) {
            (None, None) => return None,
            (a, b) if a == b => continue,
            (a, b) => {
                let (a, b) = (a.unwrap_or("(end)"), b.unwrap_or("(end)"));
                return Some(format!("want `{a}`, got `{b}`"));
            }
        }
    }
}

/// Error `err`'s first stderr line that is an error, else its first that isn't a check by
/// units' report of writes; from `tests/golden/` on when it names a golden by its full path.
fn stderr_line(err: &str) -> Option<&str> {
    let (_, s) = err.split_once("--- stderr ---\n")?;
    let mut lines = s.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = (lines.clone())
        .find(|l| l.contains("error") || l.contains("panicked"))
        .or_else(|| lines.find(|l| !l.starts_with("velt-units")))?;
    Some(line.find("tests/golden/").map_or(line, |i| &line[i..]))
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
        how_it_failed(std::slice::from_ref(&ice)),
        "tests/golden/a.velt [debug]: the build failed | error: internal compiler error: boom"
    );
    // A leak-checked golden whose build failed: the leak check's error comes first.
    let leak =
        "tests/golden/a.velt [debug]: leak check: no `rc stats` report on stderr".to_string();
    assert_eq!(
        how_it_failed(&[leak.clone(), ice]),
        "tests/golden/a.velt [debug]: the build failed | error: internal compiler error: boom"
    );
    assert_eq!(how_it_failed(std::slice::from_ref(&leak)), leak);
    let wrong = "tests/golden/b.velt [release]: exit 0 (want 0)\n--- want ---\n1\n2\n--- got ---\n1\n3\n--- stderr ---\n".to_string();
    assert_eq!(
        how_it_failed(&[wrong]),
        "tests/golden/b.velt [release]: exit 0 (want 0) | want `2`, got `3`"
    );
    let short = "tests/golden/b.velt [debug]: exit 1 (want 0)\n--- want ---\n1\n--- got ---\n--- stderr ---\n/home/ci/velt/tests/golden/b.velt:4:2: error: no\n".to_string();
    assert_eq!(
        how_it_failed(&[short]),
        "tests/golden/b.velt [debug]: exit 1 (want 0) | want `1`, got `(end)` | tests/golden/b.velt:4:2: error: no"
    );
    assert_eq!(how_it_failed(&[]), "");
}

#[test]
fn a_passing_golden_listed_units_fails_the_run() {
    let text = "tests/golden/a.velt #12\ntests/golden/c.velt #56 units\n";
    let list = parse_known_failing(text, "linux", true);
    let pass = |rel: &str| on_pass(rel, list.get(rel));
    assert_eq!(
        pass("tests/golden/c.velt"),
        OnPass::Fail(
            "tests/golden/c.velt passes: take it off golden_known_failing.txt (#56)".into()
        )
    );
    assert_eq!(
        pass("tests/golden/a.velt"),
        OnPass::Report(
            "golden: tests/golden/a.velt passes: take it off golden_known_failing.txt (#12)".into()
        )
    );
    assert_eq!(pass("tests/golden/z.velt"), OnPass::Nothing);
}
