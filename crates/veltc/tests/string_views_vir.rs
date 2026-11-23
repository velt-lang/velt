//! String views (#3469) are taken where the goldens say they are: in the release VIR of
//! `tests/golden/lang/string_views.velt`, the parsers that only slice and read their text start
//! their family with a demotion. Without this check the golden could stop exercising views and
//! still pass, since a family that keeps its clone prints the same.
//!
//! A demotion is the block that gives the member the source's words with a zero form word,
//! `m.0 = (*src as agg#0).0; m.1 = …; m.2 = 0_u64`, then jumps on (`velt_opt`'s
//! `borrowed_copies/views.rs`).

mod no_window;

use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The release VIR of the golden at `rel` (a path under the repository).
fn release_vir(rel: &str) -> String {
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--release", rel, "--emit", "vir"])
        .current_dir(repo())
        .env("VELT_BUILD_CACHE", "off")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The body of the function whose symbol is `symbol`: the lines after its `fn#N` line, up to its
/// closing brace.
fn body<'a>(vir: &'a str, symbol: &str) -> Vec<&'a str> {
    let head = format!(" {symbol}(");
    let mut lines = vir
        .lines()
        .skip_while(|l| !(l.starts_with("fn#") && l.contains(&head)));
    assert!(lines.next().is_some(), "no function {symbol} in the VIR");
    lines.take_while(|l| *l != "}").collect()
}

/// How many demotion blocks `body` has (the module doc).
fn demotions(body: &[&str]) -> usize {
    let stmts: Vec<&str> = body.iter().map(|l| l.trim()).collect();
    (0..stmts.len().saturating_sub(3))
        .filter(|&i| {
            let first = stmts[i];
            let Some((member, src)) = first.split_once(".0 = (*") else {
                return false;
            };
            src.ends_with(" as agg#0).0")
                && stmts[i + 1].starts_with(&format!("{member}.1 = "))
                && stmts[i + 2] == format!("{member}.2 = 0_u64")
                && stmts[i + 3].starts_with("goto ")
        })
        .count()
}

#[test]
fn the_parsers_of_the_views_golden_take_views() {
    let vir = release_vir("tests/golden/lang/string_views.velt");
    for symbol in ["_V4scan", "_V6commas"] {
        let n = demotions(&body(&vir, symbol));
        assert!(n >= 1, "{symbol} takes no view ({n} demotions)");
    }
}

#[test]
fn the_escape_and_suspension_goldens_take_no_views() {
    for golden in [
        "tests/golden/lang/string_views_escapes.velt",
        "tests/golden/lang/string_views_suspend.velt",
    ] {
        let vir = release_vir(golden);
        let lines: Vec<&str> = vir.lines().collect();
        assert_eq!(demotions(&lines), 0, "{golden} takes a view");
    }
}
