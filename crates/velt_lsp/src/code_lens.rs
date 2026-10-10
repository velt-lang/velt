//! Code lenses: "▶ Run" and "Debug" above a program's `main`, for the editor's run and debug
//! commands (`velt.runFile` / `velt.debugFile` in the VS Code extension, with the document's URI
//! as their argument).

use lsp_types::{CodeLens, Command, Url};
use serde_json::json;
use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::line_index::LineIndex;

/// Command the "▶ Run" lens runs.
pub const RUN_FILE: &str = "velt.runFile";
/// Command the "Debug" lens runs.
pub const DEBUG_FILE: &str = "velt.debugFile";

/// The lenses of the document: run and debug on a top-level `main` function, none otherwise.
pub fn code_lenses(analysis: &Analysis, uri: &Url) -> Vec<CodeLens> {
    let main = analysis
        .module()
        .ast
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ast::ItemKind::Function(f) if f.sig.name.name == "main" => Some(item.span),
            _ => None,
        });
    let Some(span) = main else {
        return vec![];
    };
    // On the first line of the declaration, so the lens sits above `function main`.
    let range = LineIndex::new(analysis.text()).range(span.lo, span.lo);
    [("▶ Run", RUN_FILE), ("Debug", DEBUG_FILE)]
        .into_iter()
        .map(|(title, command)| CodeLens {
            range,
            command: Some(Command {
                title: title.into(),
                command: command.into(),
                arguments: Some(vec![json!(uri)]),
            }),
            data: None,
        })
        .collect()
}
