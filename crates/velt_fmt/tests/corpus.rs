//! Formats every `.vlt` file of the repository (goldens, std) and checks the formatter's
//! guarantees: the output parses to the same AST, keeps every comment, and is a fixed point.

mod common;

use common::{check, corpus, parses};
use velt_fmt::format_source;

#[test]
fn corpus_is_idempotent_and_preserves_ast_and_comments() {
    let files = corpus();
    assert!(
        !files.is_empty(),
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
        formatted += 1;
        if let Err(msg) = check(&src) {
            failures.push(format!("{}: {msg}", path.display()));
        }
    }
    assert!(formatted > 0);
    assert!(failures.is_empty(), "\n{}\n", failures.join("\n\n"));
}
