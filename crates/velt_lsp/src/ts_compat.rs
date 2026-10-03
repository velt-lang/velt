//! Live `velt check --ts-compat` findings for documents in a package's `tsCompat` folders
//! (docs/internals/design/tsx.md "Sharing components with the client").
//!
//! The lint reuses the document's analysis: the loaded modules and the checker's diagnostics
//! ([`velt_tscompat::lint_program`]), so nothing is parsed or checked twice. Like the command, it
//! leaves a document with errors of its own alone (the rules only see valid Velt), and the files
//! in scope are the loaded ones under the folders: an import of any other file leaves the set.
//! The folders come from the package's `package.vlt`, its unsaved text when it is open, so
//! editing `tsCompat` updates the open documents.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{DiagnosticSeverity, NumberOrString};
use velt_common::{Diagnostic, FileId};
use velt_tscompat::{canonical, Finding, Severity};

use crate::analysis::Analysis;
use crate::code_actions::Fix;
use crate::line_index::LineIndex;

/// The `source` of the findings' diagnostics, which tells them from the compiler's.
pub const SOURCE: &str = "velt ts-compat";

/// The findings in the document of `analysis` (at `path`), or none when it is not in a
/// `tsCompat` folder of its package. `overlay` holds the open documents' text.
pub fn findings(
    analysis: &Analysis,
    path: &Path,
    overlay: &HashMap<PathBuf, String>,
) -> Vec<Finding> {
    let Some(folders) = folders_of(path, overlay) else {
        return vec![];
    };
    let in_folders = |p: &Path| folders.iter().any(|f| p.starts_with(f));
    if !in_folders(&canonical(path)) {
        return vec![];
    }
    let file = analysis.file();
    let mut found = velt_tscompat::lint_program(
        &analysis.modules,
        &analysis.sm,
        &analysis.diagnostics,
        &in_folders,
    );
    found.retain(|f| f.span.file == file);
    found
}

/// The canonical `tsCompat` folders of the package around `path`, if it has any.
fn folders_of(path: &Path, overlay: &HashMap<PathBuf, String>) -> Option<Vec<PathBuf>> {
    let root = vpm::manifest::find_package_root(path.parent()?)?;
    let manifest_path = root.join(vpm::manifest::MANIFEST_FILE);
    let manifest = match overlay.get(&manifest_path) {
        Some(text) => vpm::Manifest::read(FileId(0), text).ok()?,
        None => vpm::Manifest::from_path(&manifest_path).ok()?,
    };
    let folders: Vec<PathBuf> = manifest
        .ts_compat_dirs(&root)
        .iter()
        .map(|dir| canonical(dir))
        .collect();
    (!folders.is_empty()).then_some(folders)
}

/// `f` as an LSP diagnostic: its rule as the code, its notes after the message.
pub fn diagnostic(index: &LineIndex, f: &Finding) -> lsp_types::Diagnostic {
    let mut message = f.message.clone();
    for note in &f.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    lsp_types::Diagnostic {
        range: index.range(f.span.lo, f.span.hi),
        severity: Some(match f.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        code: Some(NumberOrString::String(f.code.to_string())),
        source: Some(SOURCE.into()),
        message,
        ..Default::default()
    }
}

/// Each finding's fix in the document's byte range `lo..hi`, as a preferred quick fix tied to the
/// finding's diagnostic.
pub fn fixes(analysis: &Analysis, lo: u32, hi: u32) -> Vec<Fix> {
    analysis
        .ts_compat
        .iter()
        .filter(|f| f.span.hi >= lo && f.span.lo <= hi)
        .filter_map(|f| {
            let fix = f.fix.as_ref()?;
            Some(Fix {
                title: fix.title.clone(),
                edits: vec![(fix.span, fix.replacement.clone())],
                // Matched to the client's diagnostic by range and message.
                diagnostic: Some(Diagnostic::error(f.message.clone(), f.span)),
                preferred: true,
            })
        })
        .collect()
}
