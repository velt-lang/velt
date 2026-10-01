//! The M2 golden programs (`tests/golden/m2`) with the real parser and the real std prelude:
//! programs must type-check; error goldens must fail with the expected message at the expected
//! position (and mention every other line of the `.err` file).

mod common;

use std::path::PathBuf;

use common::programs::{load_file, load_src_lenient, repo_root};

fn golden_files(dir: &str) -> Vec<PathBuf> {
    let dir = repo_root().join(dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "vlt"))
        .filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with('_'))
        .collect();
    files.sort();
    files
}

#[test]
fn m2_programs_type_check() {
    let mut failures = vec![];
    for f in golden_files("tests/golden/m2") {
        let l = load_file(&f);
        let (p, d) = l.check();
        if p.is_none() || d.iter().any(|d| d.is_error()) {
            failures.push(format!("{}:\n{}", f.display(), l.render(&d)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn m1_programs_still_type_check_with_prelude() {
    let mut failures = vec![];
    for f in golden_files("tests/golden/m1") {
        let l = load_file(&f);
        let (p, d) = l.check();
        if p.is_none() || d.iter().any(|d| d.is_error()) {
            failures.push(format!("{}:\n{}", f.display(), l.render(&d)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn m2_error_goldens() {
    let mut failures = vec![];
    for f in golden_files("tests/golden/m2/errors") {
        let l = load_file(&f);
        let (p, d) = l.check();
        let rendered = l.render(&d);
        let expected = std::fs::read_to_string(f.with_extension("err")).unwrap();
        let lines: Vec<&str> = expected
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let first = rendered.lines().next().unwrap_or("");
        let ok = p.is_none()
            && first.contains(lines[0])
            && first.contains(lines[1])
            && lines.iter().all(|l| rendered.contains(l));
        if !ok {
            failures.push(format!(
                "{}: expected {:?}\n--- got ---\n{rendered}",
                f.display(),
                lines
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Programs of later milestones use features sema rejects: diagnostics, never a panic.
#[test]
fn later_milestones_never_panic() {
    for dir in [
        "tests/golden/m3",
        "tests/golden/m4",
        "tests/golden/m3/errors",
        "tests/golden/m4/errors",
    ] {
        if !repo_root().join(dir).exists() {
            continue;
        }
        for f in golden_files(dir) {
            let l = load_file(&f);
            let _ = l.check();
        }
    }
}

/// Truncated programs (whatever AST the parser recovers) produce diagnostics, never a panic.
#[test]
fn truncated_programs_never_panic() {
    for f in golden_files("tests/golden/m2") {
        let src = std::fs::read_to_string(&f).unwrap().replace("\r\n", "\n");
        for cut in (1..src.len()).step_by(97) {
            if src.is_char_boundary(cut) {
                let _ = load_src_lenient(&src[..cut]).check();
            }
        }
    }
}
