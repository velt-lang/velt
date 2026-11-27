//! The goldens known to fail (`golden_known_failing.txt`, next to `golden.rs`): read by
//! `golden.rs`, whose failures for these only fail the run with `VELT_GOLDEN_STRICT=1`.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// An entry of the list that applies to this run.
pub struct Known {
    /// The issue tracking the failure (`#<number>`).
    pub issue: String,
    /// Marked `units` (or `units=<mode>`): it fails only in a run by units, and a run by units
    /// fails when it passes.
    pub units: bool,
}

/// The goldens known to fail on this OS, in a run by units in `VELT_UNITS`'s mode when it is set,
/// by path from the repository root.
pub fn known_failing() -> HashMap<String, Known> {
    parse_known_failing(
        include_str!("../golden_known_failing.txt"),
        std::env::consts::OS,
        units_mode(std::env::var_os("VELT_UNITS").as_deref()),
    )
}

/// The mode of a run by units that `VELT_UNITS` (`value`) asks for, as the driver and velt_sema
/// read it (`crates/veltc/src/driver.rs`, `units::stage2`): `2` with std a unit of its own, `1`
/// for any other value (std and the program in one context); `None` unset (not by units).
fn units_mode(value: Option<&std::ffi::OsStr>) -> Option<u8> {
    value.map(|v| if v == "2" { 2 } else { 1 })
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

/// The entries of known-failing list `text` that apply on `os`, in a run by units in mode `units`
/// (`VELT_UNITS`: 1 or 2) when there is one. Each line not blank and not a `#`-comment is
/// `<path> #<issue> [os=<os>,...] [units | units=1 | units=2]`: an entry marked `units` fails
/// only in a run by units (#2943), in either mode; `units=<mode>` only in that mode. Anything
/// else is a mistake in the list.
fn parse_known_failing(text: &str, os: &str, units: Option<u8>) -> HashMap<String, Known> {
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
        // The modes of a run by units the entry applies in (none: it isn't marked `units`).
        let (mut on_os, mut modes): (bool, &[u8]) = (true, &[]);
        for w in words.by_ref() {
            match (w.strip_prefix("os="), w) {
                (Some(list), _) => on_os = list.split(',').any(|o| o == os),
                (None, "units") => modes = &[1, 2],
                (None, "units=1") => modes = &[1],
                (None, "units=2") => modes = &[2],
                _ => panic!(
                    "golden_known_failing.txt: `{line}`: after the issue only `os=...` and `units` \
                     (or `units=1`, `units=2`)"
                ),
            }
        }
        let by_units = !modes.is_empty();
        if on_os && (!by_units || units.is_some_and(|m| modes.contains(&m))) {
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
    let mac = parse_known_failing(text, "macos", None);
    assert_eq!(
        mac.get("tests/golden/a.velt").map(|k| k.issue.as_str()),
        Some("#12")
    );
    assert!(!mac.contains_key("tests/golden/b.velt"));
    assert!(!mac.contains_key("tests/golden/c.velt"));
    let linux = parse_known_failing(text, "linux", None);
    assert_eq!(
        linux.get("tests/golden/b.velt").map(|k| k.issue.as_str()),
        Some("#34")
    );
    assert!(!linux.contains_key("tests/golden/d.velt"));
    // An entry marked `units` applies in a run by units only, on its OSes.
    let mac_units = parse_known_failing(text, "macos", Some(2));
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
    let linux_units = parse_known_failing(text, "linux", Some(2));
    assert!(linux_units.contains_key("tests/golden/d.velt"));
    // The words after the issue in either order.
    let either = parse_known_failing(
        "tests/golden/e.velt #90 units os=windows\n",
        "windows",
        Some(2),
    );
    assert!(either.contains_key("tests/golden/e.velt"));
    let either = parse_known_failing(
        "tests/golden/e.velt #90 units os=windows\n",
        "linux",
        Some(2),
    );
    assert!(!either.contains_key("tests/golden/e.velt"));
    // Every entry of the real list names a golden that is there, on any OS, in either mode.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let all = include_str!("../golden_known_failing.txt");
    for os in ["macos", "linux", "windows"] {
        for mode in [1, 2] {
            for path in parse_known_failing(all, os, Some(mode)).keys() {
                assert!(
                    root.join(path).is_file(),
                    "golden_known_failing.txt: no golden {path}"
                );
            }
        }
    }
}

/// `units` applies in a run by units in either mode (`VELT_UNITS=1`: std and the program in one
/// context; `VELT_UNITS=2`: std a unit of its own), `units=<mode>` in that mode only.
#[test]
fn the_known_failing_list_reads_the_mode_of_a_run_by_units() {
    let text = "tests/golden/a.velt #1\ntests/golden/b.velt #2 units\ntests/golden/c.velt #3 units=1\ntests/golden/d.velt #4 os=linux units=2\n";
    let keys = |mode: Option<u8>| {
        let mut k: Vec<String> = parse_known_failing(text, "linux", mode)
            .into_keys()
            .collect();
        k.sort();
        k
    };
    let paths = |names: &[&str]| -> Vec<String> {
        (names.iter())
            .map(|n| format!("tests/golden/{n}.velt"))
            .collect()
    };
    assert_eq!(keys(None), paths(&["a"]));
    assert_eq!(keys(Some(1)), paths(&["a", "b", "c"]));
    assert_eq!(keys(Some(2)), paths(&["a", "b", "d"]));
    // A `units=<mode>` entry fails the run when it passes in its mode, as a `units` one does.
    let one = parse_known_failing(text, "linux", Some(1));
    assert!(matches!(
        on_pass("tests/golden/c.velt", one.get("tests/golden/c.velt")),
        OnPass::Fail(_)
    ));
    // `VELT_UNITS` as the driver reads it: `2`, or any other value for one context.
    let mode = |s: &str| units_mode(Some(std::ffi::OsStr::new(s)));
    assert_eq!(units_mode(None), None);
    assert_eq!(mode("2"), Some(2));
    assert_eq!(mode("1"), Some(1));
    assert_eq!(mode("yes"), Some(1));
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
    let list = parse_known_failing(text, "linux", Some(2));
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
