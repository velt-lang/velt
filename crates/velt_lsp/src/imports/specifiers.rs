//! Completion of module specifiers inside `from "…"`: the standard library's modules and the
//! dependencies (from the loader's [`crate::ProgramLoader::module_index`]), and the files and
//! folders next to the document for `./` and `../`, named as the loader resolves them (no
//! extension unless two files share a name).

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
    let names: Vec<&str> = paths
        .iter()
        .filter(|p| !p.is_dir())
        .filter_map(|p| p.file_name()?.to_str())
        .collect();
    for path in &paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if path.is_dir() {
            if vpm::sources::walks_into(path) {
                out.push((format!("{prefix}{name}/"), true, String::new()));
            }
        } else if vpm::sources::walk_keeps(name) && path != doc {
            let spelled = file_spelling(name, &names);
            out.push((format!("{prefix}{spelled}"), false, name.to_string()));
        }
    }
    out
}

/// How an import names the source file `name` among the files `siblings` of its folder: without
/// its extension, unless another source file shares its stem (`./foo` would be ambiguous).
fn file_spelling<'a>(name: &'a str, siblings: &[&str]) -> &'a str {
    let Some(stem) = vpm::sources::strip_source_extension(name) else {
        return name;
    };
    let shared = siblings
        .iter()
        .any(|s| *s != name && vpm::sources::strip_source_extension(s) == Some(stem));
    if shared {
        name
    } else {
        stem
    }
}

/// The relative specifier that names the source file `file` from the document at `doc`, as the
/// loader resolves relative imports: the extension stays when another file shares the stem; a
/// folder module (`shapes/index.vlt`) is named by its folder (`./shapes`) unless a file module of
/// that name would win over it or the folder has no name to spell (`../index`).
pub fn relative_spec(doc: &Path, file: &Path) -> Option<String> {
    let doc_dir = doc.parent()?;
    let dir = file.parent()?;
    let name = file.file_name()?.to_str()?;
    let read = std::fs::read_dir(dir).ok()?;
    let siblings: Vec<String> = read
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let siblings: Vec<&str> = siblings.iter().map(String::as_str).collect();
    let spelled = file_spelling(name, &siblings);
    let rel_dir = vpm::relpath::relative(dir, doc_dir);
    if spelled == "index" {
        if let Some(folder) = folder_spec(dir, &rel_dir) {
            return Some(folder);
        }
    }
    Some(match rel_dir.as_str() {
        "." => format!("./{spelled}"),
        _ if rel_dir.starts_with("..") => format!("{rel_dir}/{spelled}"),
        _ => format!("./{rel_dir}/{spelled}"),
    })
}

/// The specifier naming the folder `dir` (`rel_dir` from the document's folder) as a folder
/// module, unless it has no name there (`.`, `..`) or a file module of its name hides it.
fn folder_spec(dir: &Path, rel_dir: &str) -> Option<String> {
    let last = rel_dir.rsplit('/').next()?;
    if last == "." || last == ".." {
        return None;
    }
    let folder = dir.file_name()?.to_str()?;
    let parent = dir.parent()?;
    let hidden = vpm::sources::source_files(folder)
        .iter()
        .any(|f| parent.join(f).is_file());
    if hidden {
        return None;
    }
    Some(if rel_dir.starts_with("..") {
        rel_dir.to_string()
    } else {
        format!("./{rel_dir}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_specs_name_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for f in [
            "app/main.vlt",
            "app/util.vlt",
            "app/dup.vlt",
            "app/dup.ts",
            "app/shapes/index.vlt",
            "app/hidden.vlt",
            "app/hidden/index.vlt",
            "index.vlt",
            "lib/math.ts",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let doc = root.join("app/main.vlt");
        let spec = |f: &str| relative_spec(&doc, &root.join(f)).unwrap();
        assert_eq!(spec("app/util.vlt"), "./util");
        assert_eq!(spec("app/dup.ts"), "./dup.ts");
        assert_eq!(spec("app/shapes/index.vlt"), "./shapes");
        assert_eq!(spec("app/hidden/index.vlt"), "./hidden/index");
        assert_eq!(spec("index.vlt"), "../index");
        assert_eq!(spec("lib/math.ts"), "../lib/math");
    }
}
