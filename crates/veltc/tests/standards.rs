//! Enforces the file-size rule from CONTRIBUTING.md "Coding standards" across the workspace.
//! The rule itself lives in crates/xtask/src/file_size.rs, which `cargo xtask precheck` also
//! uses on the changed files.

use std::path::{Path, PathBuf};

#[path = "../../xtask/src/file_size.rs"]
mod file_size;

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
        if !file_size::applies(&rel) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        violations.extend(file_size::violation(&rel, &text));
    }
    assert!(violations.is_empty(), "\n{}\n", violations.join("\n"));
}
