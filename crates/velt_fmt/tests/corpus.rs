//! Formats every `.vlt` file of the repository (goldens, std) and checks the formatter's
//! guarantees: the output parses to the same AST, keeps every comment, and is a fixed point.

mod common;

use common::{changed_files, check, corpus, parses, root};
use velt_fmt::format_source;

#[test]
fn corpus_is_idempotent_and_preserves_ast_and_comments() {
    let files = corpus();
    let only_changed = std::env::var("VELT_FMT_CORPUS").is_ok_and(|v| v == "changed");
    assert!(
        only_changed || !files.is_empty(),
        "no .vlt files found under tests/golden or std"
    );
    let mut failures = vec![];
    let mut formatted = 0;
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        if !parses(&src) {
            assert!(
                format_source(&src).is_err(),
                "{}: accepted a parse error",
                path.display()
            );
            continue;
        }
        if velt_syntax::first_this_param(&src).is_some() {
            assert!(
                format_source(&src).is_err(),
                "{}: formatted a file with a `this` parameter",
                path.display()
            );
            continue;
        }
        formatted += 1;
        if let Err(msg) = check(&src) {
            failures.push(format!("{}: {msg}", path.display()));
        }
    }
    assert!(only_changed || formatted > 0);
    assert!(failures.is_empty(), "\n{}\n", failures.join("\n\n"));
}

/// A change's list: the files that still exist, relative to the workspace root.
#[test]
fn the_changed_corpus_is_the_listed_files_that_exist() {
    let files =
        changed_files(" tests/golden/m1/hello.vlt, ,tests/golden/deleted_by_the_change.vlt");
    assert_eq!(files, [root().join("tests/golden/m1/hello.vlt")]);
}
