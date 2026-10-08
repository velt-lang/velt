//! std keeps its documentation: no declaration that `velt doc` documents has an empty doc
//! while a plain `//` comment ends right above it. Plain comments stopped being documentation
//! with `/** */` (docs/internals/design/doc-comments.md), so such a comment is a doc comment
//! written in the old style, and the API reference would silently lose it.

use std::path::{Path, PathBuf};

fn vlt_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read std")
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            vlt_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "vlt") {
            out.push(path);
        }
    }
}

#[test]
fn std_declarations_have_doc_comments_not_plain_comments() {
    let std_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let mut files = vec![];
    vlt_files(&std_dir, &mut files);
    assert!(files.len() > 50, "{} std files", files.len());
    let mut lost = vec![];
    for file in &files {
        let src = std::fs::read_to_string(file).expect("read std file");
        // Line endings may be CRLF in a Windows checkout; offsets are the same either way.
        for lo in velt_doc::extract::plain_comments_above_docs(&src) {
            let line = src[..lo as usize].matches('\n').count() + 1;
            let rel = file.strip_prefix(&std_dir).unwrap_or(file);
            lost.push(format!("std/{}:{line}", rel.display()).replace('\\', "/"));
        }
    }
    assert!(
        lost.is_empty(),
        "these declarations are documented by `velt doc` but have a plain `//` comment right \
         above them instead of a `/** … */` doc comment:\n{}",
        lost.join("\n")
    );
}
