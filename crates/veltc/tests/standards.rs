//! Enforces the file-size rule from CLAUDE.md "Coding standards" across the workspace.
//!

use std::path::{Path, PathBuf};

const SRC_LIMIT: usize = 800;
const TEST_LIMIT: usize = 1000;

/// Files temporarily over the limit, with the milestone that must split them.
const ALLOWED_OVERSIZE: &[(&str, &str)] = &[];

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "target") {
                collect_rs(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn source_files_stay_small() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut files = vec![];
    collect_rs(&root.join("crates"), &mut files);

    let mut violations = vec![];
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED_OVERSIZE.iter().any(|(path, _)| *path == rel) {
            continue;
        }
        let is_test = rel.contains("/tests/");
        let limit = if is_test { TEST_LIMIT } else { SRC_LIMIT };
        let lines = std::fs::read_to_string(&file).unwrap().lines().count();
        if lines > limit {
            violations.push(format!(
                "{rel}: {lines} lines (limit {limit}) — split it by concern"
            ));
        }
    }
    assert!(violations.is_empty(), "\n{}\n", violations.join("\n"));
}
