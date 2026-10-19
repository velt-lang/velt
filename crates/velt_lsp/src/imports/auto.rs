//! Auto-import: the exports of the std modules, the dependencies and the package's files that
//! the document does not import, as completions carrying the edit that imports them, and as
//! "Import `x` from `<spec>`" fixes for the "cannot find `x`" errors.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lsp_types::{CompletionItem, CompletionItemLabelDetails, TextEdit};

use super::exports::{Export, Resolve};
use super::{
    edit, specifiers, Candidate, FileExports, ImportHelp, Source, StdExports, MAX_AUTO_IMPORTS,
    MAX_IMPORT_FIXES,
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
        let lower = prefix.to_lowercase();
        let wanted =
            |e: &Export| e.name.to_lowercase().starts_with(&lower) && !in_scope.contains(&e.name);
        let mut found = self.candidates(loader, doc, package, &wanted);
        // Best first, then cut: names starting with exactly what was typed, shorter names, the
        // package's own modules before dependencies before std.
        found.sort_by_cached_key(|c| {
            let name = &c.export.name;
            (!name.starts_with(prefix), name.len(), c.rank, name.clone())
        });
        let capped = found.len() > MAX_AUTO_IMPORTS;
        found.truncate(MAX_AUTO_IMPORTS);
        let index = LineIndex::new(analysis.text());
        let items = found
            .into_iter()
            .filter_map(|c| {
                let spec = c.spec(loader, doc)?;
                let (span, text) =
                    edit::import_edit(analysis, &spec, &c.export.name, c.export.is_type);
                let mut item = CompletionItem {
                    label: c.export.name.clone(),
                    kind: Some(c.export.kind),
                    label_details: Some(CompletionItemLabelDetails {
                        detail: None,
                        description: Some(spec.clone()),
                    }),
                    detail: Some(if c.export.detail.is_empty() {
                        format!("import {{ {} }} from \"{spec}\"", c.export.name)
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
                };
                c.export.document(&mut item);
                Some(item)
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
            found.sort_by_key(|c| c.rank);
            found.truncate(MAX_IMPORT_FIXES);
            let specs: Vec<(String, bool)> = found
                .iter()
                .filter_map(|c| Some((c.spec(loader, doc)?, c.export.is_type)))
                .collect();
            let preferred = specs.len() == 1;
            out.extend(specs.into_iter().map(|(spec, is_type)| Fix {
                title: format!("Import `{name}` from `{spec}`"),
                edits: vec![edit::import_edit(analysis, &spec, name, is_type)],
                diagnostic: Some(d.clone()),
                preferred,
            }));
        }
        out
    }

    /// The exports `wanted` takes, from the std modules, the dependencies and the package's
    /// files, once per name and module.
    fn candidates(
        &mut self,
        loader: &dyn ProgramLoader,
        doc: &Path,
        package: &[FileExports],
        wanted: &dyn Fn(&Export) -> bool,
    ) -> Vec<Candidate> {
        let entries = loader.module_index(doc);
        let resolve = |spec: &str, from: &Path| loader.resolve_module(spec, from);
        let mut out = vec![];
        let take = |source: Source, rank: u8, exports: &[Export], out: &mut Vec<Candidate>| {
            let mut seen = HashSet::new();
            out.extend(
                exports
                    .iter()
                    .filter(|e| wanted(e) && seen.insert(e.name.clone()))
                    .map(|e| Candidate {
                        source: source.clone(),
                        rank,
                        export: e.clone(),
                    }),
            );
        };
        for (path, exports) in package {
            if *path != doc {
                take(Source::File(path.to_path_buf()), 0, exports, &mut out);
            }
        }
        for entry in entries.iter().filter(|e| e.kind == ModuleKind::Dependency) {
            let exports = self.exports.exports(&entry.path, &resolve);
            take(Source::Module(entry.spec.clone()), 1, &exports, &mut out);
        }
        for (spec, exports) in self.std_exports(&entries, &resolve).iter() {
            take(Source::Module(spec.clone()), 2, exports, &mut out);
        }
        out
    }

    /// The exports of the std modules among `entries`, parsed once per std root.
    fn std_exports(&mut self, entries: &[ModuleEntry], resolve: Resolve) -> StdExports {
        let Some(root) = std_root(entries) else {
            return Arc::default();
        };
        if let Some((cached_root, exports)) = &self.std {
            if *cached_root == root {
                return exports.clone();
            }
        }
        let exports: Vec<(String, Vec<Export>)> = entries
            .iter()
            .filter(|e| e.kind == ModuleKind::Std)
            .map(|e| {
                (
                    e.spec.clone(),
                    self.exports.exports(&e.path, resolve).to_vec(),
                )
            })
            .collect();
        let exports = Arc::new(exports);
        self.std = Some((root, exports.clone()));
        exports
    }
}

impl Candidate {
    /// The specifier the document at `doc` imports this candidate's module by; `None` for a file
    /// that specifier would not load.
    fn spec(&self, loader: &dyn ProgramLoader, doc: &Path) -> Option<String> {
        match &self.source {
            Source::Module(spec) => Some(spec.clone()),
            Source::File(file) => {
                let spec = specifiers::relative_spec(doc, file)?;
                // The loader has the last word where it can tell.
                match loader.resolve_module(&spec, doc) {
                    Some(found) => same_file(&found, file).then_some(spec),
                    None => (!loader.resolves_modules()).then_some(spec),
                }
            }
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

/// The std root the std entries' files are in (`velt:fs` → `<root>/fs.vlt`).
fn std_root(entries: &[ModuleEntry]) -> Option<PathBuf> {
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
