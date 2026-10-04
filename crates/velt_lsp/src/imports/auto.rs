//! Auto-import: the exports of the std modules, the dependencies and the package's files that
//! the document does not import, as completions carrying the edit that imports them, and as
//! "Import `x` from `<spec>`" fixes for the "cannot find `x`" errors.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lsp_types::{CompletionItem, CompletionItemLabelDetails, TextEdit};

use super::exports::Export;
use super::{
    edit, Candidate, FileExports, ImportHelp, StdExports, MAX_AUTO_IMPORTS, MAX_IMPORT_FIXES,
};
use crate::analysis::Analysis;
use crate::code_actions::Fix;
use crate::line_index::LineIndex;
use crate::{ModuleEntry, ModuleKind, ProgramLoader};

impl ImportHelp {
    /// Auto-import completions for the word `prefix` (not empty) at the cursor: the exports
    /// starting with it (ignoring case) that are not among `in_scope`, each with the edit
    /// importing it. The flag says whether the list was cut at [`MAX_AUTO_IMPORTS`].
    pub fn auto_imports(
        &mut self,
        analysis: &Analysis,
        loader: &dyn ProgramLoader,
        doc: &Path,
        package: &[FileExports],
        prefix: &str,
        in_scope: &HashSet<String>,
    ) -> (Vec<CompletionItem>, bool) {
        let prefix = prefix.to_lowercase();
        let wanted =
            |e: &Export| e.name.to_lowercase().starts_with(&prefix) && !in_scope.contains(&e.name);
        let mut found = self.candidates(loader, doc, package, &wanted);
        let capped = found.len() > MAX_AUTO_IMPORTS;
        found.truncate(MAX_AUTO_IMPORTS);
        let index = LineIndex::new(analysis.text());
        let items = found
            .into_iter()
            .map(|c| {
                let (span, text) =
                    edit::import_edit(analysis, &c.spec, &c.export.name, c.export.is_type);
                CompletionItem {
                    label: c.export.name.clone(),
                    kind: Some(c.export.kind),
                    label_details: Some(CompletionItemLabelDetails {
                        detail: None,
                        description: Some(c.spec.clone()),
                    }),
                    detail: Some(if c.export.detail.is_empty() {
                        format!("import {{ {} }} from \"{}\"", c.export.name, c.spec)
                    } else {
                        c.export.detail.clone()
                    }),
                    // After the names in scope.
                    sort_text: Some(format!("~{}", c.export.name)),
                    additional_text_edits: Some(vec![TextEdit::new(
                        index.range(span.lo, span.hi),
                        text,
                    )]),
                    ..Default::default()
                }
            })
            .collect();
        (items, capped)
    }

    /// "Import `x` from `<spec>`" fixes for the "cannot find `x`" errors in the document's byte
    /// range `lo..hi`.
    pub fn fixes(
        &mut self,
        analysis: &Analysis,
        loader: &dyn ProgramLoader,
        doc: &Path,
        package: &[FileExports],
        (lo, hi): (u32, u32),
    ) -> Vec<Fix> {
        let mut out = vec![];
        for d in &analysis.diagnostics {
            let Some(span) = d.labels.first().map(|l| l.span) else {
                continue;
            };
            if span.file != analysis.file() || span.hi < lo || span.lo > hi {
                continue;
            }
            let Some((name, wants_type)) = unknown_name(&d.message) else {
                continue;
            };
            let wanted = |e: &Export| e.name == name && (!wants_type || e.is_type);
            let mut found = self.candidates(loader, doc, package, &wanted);
            found.truncate(MAX_IMPORT_FIXES);
            let preferred = found.len() == 1;
            out.extend(found.into_iter().map(|c| Fix {
                title: format!("Import `{name}` from `{}`", c.spec),
                edits: vec![edit::import_edit(analysis, &c.spec, name, c.export.is_type)],
                diagnostic: Some(d.clone()),
                preferred,
            }));
        }
        out
    }

    /// The exports `wanted` takes, from the std modules, the dependencies and the package's
    /// files (in that order, each sorted by name), once per name and module.
    fn candidates(
        &mut self,
        loader: &dyn ProgramLoader,
        doc: &Path,
        package: &[FileExports],
        wanted: &dyn Fn(&Export) -> bool,
    ) -> Vec<Candidate> {
        let entries = loader.module_index(doc);
        let std_root = std_root(&entries);
        let mut out = vec![];
        let take = |spec: &str, exports: &[Export], out: &mut Vec<Candidate>| {
            let mut seen = HashSet::new();
            let mut found: Vec<Candidate> = exports
                .iter()
                .filter(|e| wanted(e) && seen.insert(e.name.clone()))
                .map(|e| Candidate {
                    spec: spec.to_string(),
                    export: e.clone(),
                })
                .collect();
            found.sort_by(|a, b| a.export.name.cmp(&b.export.name));
            out.extend(found);
        };
        for (spec, exports) in self.std_exports(&entries, std_root.as_deref()).iter() {
            take(spec, exports, &mut out);
        }
        for entry in entries.iter().filter(|e| e.kind == ModuleKind::Dependency) {
            let exports = self.exports.exports(&entry.path, std_root.as_deref());
            take(&entry.spec, &exports, &mut out);
        }
        for (path, exports) in package {
            if *path != doc {
                if let Some(spec) = relative_spec(doc, path) {
                    take(&spec, exports, &mut out);
                }
            }
        }
        out
    }

    /// The exports of the std modules among `entries`, parsed once per std root.
    fn std_exports(&mut self, entries: &[ModuleEntry], std_root: Option<&Path>) -> StdExports {
        let Some(root) = std_root else {
            return Arc::default();
        };
        if let Some((cached_root, exports)) = &self.std {
            if cached_root == root {
                return exports.clone();
            }
        }
        let exports: Vec<(String, Vec<Export>)> = entries
            .iter()
            .filter(|e| e.kind == ModuleKind::Std)
            .map(|e| (e.spec.clone(), self.exports.exports(&e.path, Some(root))))
            .collect();
        let exports = Arc::new(exports);
        self.std = Some((root.to_path_buf(), exports.clone()));
        exports
    }
}

/// The std root the std entries' files are in (`velt:fs` → `<root>/fs.vlt`).
pub(super) fn std_root(entries: &[ModuleEntry]) -> Option<PathBuf> {
    let entry = entries.iter().find(|e| e.kind == ModuleKind::Std)?;
    let rel = entry.spec.strip_prefix("velt:")?;
    let mut levels = rel.split('/').count();
    if entry.path.file_stem().is_some_and(|s| s == "index") && !rel.ends_with("index") {
        levels += 1;
    }
    let mut root = entry.path.as_path();
    for _ in 0..levels {
        root = root.parent()?;
    }
    Some(root.to_path_buf())
}

/// The relative specifier the document at `doc` imports the file at `file` by (`./util`,
/// `../lib/math`, `./shapes` for `shapes/index.vlt`).
fn relative_spec(doc: &Path, file: &Path) -> Option<String> {
    let rel = vpm::relpath::relative(file, doc.parent()?);
    let rel = rel.replace('\\', "/");
    let module = vpm::sources::strip_source_extension(&rel)?;
    let module = module.strip_suffix("/index").unwrap_or(module);
    Some(if module.starts_with("../") {
        module.to_string()
    } else {
        format!("./{module}")
    })
}

/// Whether a "cannot find `x`" error is in the document's byte range `lo..hi`.
pub fn has_unknown_names(analysis: &Analysis, lo: u32, hi: u32) -> bool {
    analysis.diagnostics.iter().any(|d| {
        d.labels
            .first()
            .is_some_and(|l| l.span.file == analysis.file() && l.span.hi >= lo && l.span.lo <= hi)
            && unknown_name(&d.message).is_some()
    })
}

/// The name of a "cannot find `x` in this scope" error (sema's messages), and whether it must
/// be a type (`cannot find type/class/struct `X``).
fn unknown_name(message: &str) -> Option<(&str, bool)> {
    let rest = message
        .strip_prefix("cannot find ")?
        .strip_suffix("` in this scope")?;
    match rest.split_once(" `") {
        Some((_, name)) => Some((name, true)),
        None => Some((rest.strip_prefix('`')?, false)),
    }
}
