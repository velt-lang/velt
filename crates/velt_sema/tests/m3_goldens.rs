//! The M3/M4 golden programs (`tests/golden/m3`, `tests/golden/m4`) with the real parser and
//! the real std (prelude + imported modules): programs must type-check; error goldens must fail
//! with the expected message at the expected position (and mention every other `.err` line).

mod common;

use std::path::PathBuf;

use common::programs::{load_file, repo_root};

fn golden_files(dir: &str) -> Vec<PathBuf> {
    let dir = repo_root().join(dir);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "vlt"))
        .filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with('_'))
        .collect();
    files.sort();
    files
}

fn programs_type_check(dir: &str) {
    let mut failures = vec![];
    for f in golden_files(dir) {
        let l = load_file(&f);
        let (p, d) = l.check();
        if p.is_none() || d.iter().any(|d| d.is_error()) {
            failures.push(format!("{}:\n{}", f.display(), l.render(&d)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn error_goldens(dir: &str) {
    let mut failures = vec![];
    for f in golden_files(dir) {
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

#[test]
fn m3_programs_type_check() {
    programs_type_check("tests/golden/m3");
}

#[test]
fn m4_programs_type_check() {
    programs_type_check("tests/golden/m4");
}

#[test]
fn m3_error_goldens() {
    error_goldens("tests/golden/m3/errors");
}

#[test]
fn m4_error_goldens() {
    error_goldens("tests/golden/m4/errors");
}
