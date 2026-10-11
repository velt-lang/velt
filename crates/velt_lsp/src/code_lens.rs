//! Code lenses for the editor's run and debug commands:
//! - "▶ Run" and "Debug" above a program's `main` (`velt.runFile` / `velt.debugFile` in the VS
//!   Code extension, with the document's URI as their argument);
//! - "▶ Run test" and "Debug test" above each test of a test file (`velt.runTest` /
//!   `velt.debugTest`, with the URI and the test's name): an exported `test_*` function without
//!   parameters or generics in a `*.test.vlt` (`.ts`, `.tsx`) file, as `velt test` finds them.

use lsp_types::{CodeLens, Command, Range, Url};
use serde_json::{json, Value};
use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::line_index::LineIndex;

/// Command the "▶ Run" lens runs.
pub const RUN_FILE: &str = "velt.runFile";
/// Command the "Debug" lens runs.
pub const DEBUG_FILE: &str = "velt.debugFile";
/// Command the "▶ Run test" lens runs.
pub const RUN_TEST: &str = "velt.runTest";
/// Command the "Debug test" lens runs.
pub const DEBUG_TEST: &str = "velt.debugTest";

/// The lenses of the document: run and debug on a top-level `main` function, and on each test of
/// a test file.
pub fn code_lenses(analysis: &Analysis, uri: &Url) -> Vec<CodeLens> {
    let index = LineIndex::new(analysis.text());
    // On the first line of the declaration, so the lens sits above it.
    let at = |item: &ast::Item| index.range(item.span.lo, item.span.lo);
    let test_file = is_test_file(uri);
    let mut lenses = vec![];
    for item in &analysis.module().ast.items {
        let ast::ItemKind::Function(f) = &item.kind else {
            continue;
        };
        let name = &f.sig.name.name;
        if name == "main" {
            let args = vec![json!(uri)];
            lenses.push(lens(at(item), "▶ Run", RUN_FILE, args.clone()));
            lenses.push(lens(at(item), "Debug", DEBUG_FILE, args));
        } else if test_file
            && name.starts_with("test_")
            && item.exported
            && f.sig.params.is_empty()
            && f.sig.generics.is_empty()
        {
            let args = vec![json!(uri), json!(name)];
            lenses.push(lens(at(item), "▶ Run test", RUN_TEST, args.clone()));
            lenses.push(lens(at(item), "Debug test", DEBUG_TEST, args));
        }
    }
    lenses
}

fn lens(range: Range, title: &str, command: &str, arguments: Vec<Value>) -> CodeLens {
    CodeLens {
        range,
        command: Some(Command {
            title: title.into(),
            command: command.into(),
            arguments: Some(arguments),
        }),
        data: None,
    }
}

/// A test file: a source module whose name without the extension ends in `.test`.
fn is_test_file(uri: &Url) -> bool {
    let name = uri.path().rsplit('/').next().unwrap_or_default();
    vpm::sources::strip_source_extension(name).is_some_and(|stem| stem.ends_with(".test"))
}
