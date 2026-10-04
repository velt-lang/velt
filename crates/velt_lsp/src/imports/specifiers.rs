//! Completion of module specifiers inside `from "…"`: the standard library's modules and the
//! dependencies (from the loader's [`crate::ProgramLoader::module_index`]), and the files and
//! folders next to the document for `./` and `../`, named as the loader resolves them (no
//! extension unless two files share a name).

use std::collections::HashMap;
use std::path::Path;

use lsp_types::{CompletionItem, CompletionItemKind, CompletionTextEdit, TextEdit};

use crate::line_index::LineIndex;
use crate::{ModuleEntry, ModuleKind};

/// At most this many entries of one folder are offered.
const MAX_FILES: usize = 500;

/// Completion items for a specifier whose contents `lo..hi` start with `typed` (before the
/// cursor), in the document at `doc`.
pub fn items(
    entries: &[ModuleEntry],
    doc: &Path,
    text: &str,
    typed: &str,
    (lo, hi): (u32, u32),
) -> Vec<CompletionItem> {
    let range = LineIndex::new(text).range(lo, hi);
    let item = |spec: String, kind, detail: String| CompletionItem {
        label: spec.clone(),
        kind: Some(kind),
        detail: (!detail.is_empty()).then_some(detail),
        filter_text: Some(spec.clone()),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(range, spec))),
        ..Default::default()
    };
    let mut out = vec![];
    if typed.is_empty() || typed.starts_with('.') {
        for (spec, is_dir, detail) in relative(doc, typed) {
            let kind = if is_dir {
                CompletionItemKind::FOLDER
            } else {
                CompletionItemKind::FILE
            };
            out.push(item(spec, kind, detail));
        }
    }
    if !typed.starts_with('.') {
        for entry in entries {
            let detail = match entry.kind {
                ModuleKind::Std => entry.doc.clone(),
                ModuleKind::Dependency => "dependency".to_string(),
            };
            out.push(item(entry.spec.clone(), CompletionItemKind::MODULE, detail));
        }
    }
    out
}

/// `(specifier, is a folder, file name)` of the entries of the folder `typed` names (up to its
/// last `/`; `./` when it has none), relative to the document's folder.
fn relative(doc: &Path, typed: &str) -> Vec<(String, bool, String)> {
    let Some(doc_dir) = doc.parent() else {
        return vec![];
    };
    let prefix = match typed.rfind('/') {
        Some(i) => &typed[..=i],
        None => "./",
    };
    let dir = vpm::relpath::normalize(&doc_dir.join(prefix));
    let Ok(read) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut paths: Vec<_> = read.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    paths.truncate(MAX_FILES);
    let mut out = vec![];
    if prefix == "./" {
        out.push(("../".to_string(), true, String::new()));
    }
    let mut files = vec![];
    for path in &paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if path.is_dir() {
            if vpm::sources::walks_into(path) {
                out.push((format!("{prefix}{name}/"), true, String::new()));
            }
        } else if vpm::sources::walk_keeps(name) && path != doc {
            files.push(name);
        }
    }
    let mut stems: HashMap<&str, usize> = HashMap::new();
    for name in &files {
        let stem = vpm::sources::strip_source_extension(name).unwrap_or(name);
        *stems.entry(stem).or_default() += 1;
    }
    for name in files {
        let stem = vpm::sources::strip_source_extension(name).unwrap_or(name);
        let spelled = if stems[stem] > 1 { name } else { stem };
        out.push((format!("{prefix}{spelled}"), false, name.to_string()));
    }
    out
}
