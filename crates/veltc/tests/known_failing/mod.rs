//! The goldens known to fail (`golden_known_failing.txt`, next to `golden.rs`): read by
//! `golden.rs`, whose failures for these only fail the run with `VELT_GOLDEN_STRICT=1`.

use std::collections::HashMap;
use std::path::Path;

/// The goldens known to fail on this OS, in a run by units when `VELT_UNITS=2`: path from the
/// repository root, then the issue tracking it.
pub fn known_failing() -> HashMap<String, String> {
    parse_known_failing(
        include_str!("../golden_known_failing.txt"),
        std::env::consts::OS,
        std::env::var_os("VELT_UNITS").is_some_and(|v| v == "2"),
    )
}

/// The entries of known-failing list `text` that apply on `os`, in a run by units when `units`
/// (`VELT_UNITS=2`). Each line not blank and not a `#`-comment is
/// `<path> #<issue> [os=<os>,...] [units]`: an entry marked `units` fails only in a run by units
/// (#2943); anything else is a mistake in the list.
fn parse_known_failing(text: &str, os: &str, units: bool) -> HashMap<String, String> {
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
            out.insert(path.to_string(), issue.to_string());
        }
    }
    out
}

#[test]
fn the_known_failing_list_reads_its_entries_by_os() {
    let text = "# comment\n\ntests/golden/a.velt #12\ntests/golden/b.velt #34 os=windows,linux\ntests/golden/c.velt #56 units\ntests/golden/d.velt #78 os=linux units\n";
    let mac = parse_known_failing(text, "macos", false);
    assert_eq!(
        mac.get("tests/golden/a.velt").map(String::as_str),
        Some("#12")
    );
    assert!(!mac.contains_key("tests/golden/b.velt"));
    assert!(!mac.contains_key("tests/golden/c.velt"));
    let linux = parse_known_failing(text, "linux", false);
    assert_eq!(
        linux.get("tests/golden/b.velt").map(String::as_str),
        Some("#34")
    );
    assert!(!linux.contains_key("tests/golden/d.velt"));
    // An entry marked `units` applies in a run by units only, on its OSes.
    let mac_units = parse_known_failing(text, "macos", true);
    assert_eq!(
        mac_units.get("tests/golden/c.velt").map(String::as_str),
        Some("#56")
    );
    assert!(!mac_units.contains_key("tests/golden/d.velt"));
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
