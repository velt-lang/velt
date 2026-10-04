//! The exports of modules that are not (or not yet) part of the document's program, read by
//! parsing alone: their names, kinds and signatures, for completion inside `import { … }` and
//! auto-import. Re-exports (`export { a } from "…"`, `export * from "…"`) are followed through
//! `velt:` and relative specifiers. Parsed files are cached by modification time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use lsp_types::CompletionItemKind;
use velt_common::{FileId, SourceMap};
use velt_sema::SourceModule;
use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::index::{self, Decl};
use crate::{completion, signature};

/// How deep re-exports are followed (guards against cycles).
const MAX_REEXPORT_DEPTH: usize = 8;

/// One exported name.
#[derive(Clone, Debug)]
pub struct Export {
    /// The exported name.
    pub name: String,
    /// What it is, as a completion kind.
    pub kind: CompletionItemKind,
    /// Its signature (empty for a re-export whose module could not be read).
    pub detail: String,
    /// Whether it names a type (class, struct, interface, enum, type alias).
    pub is_type: bool,
}

/// What one file exports: its own declarations (local export lists included) and its re-exports.
#[derive(Debug, Default)]
pub struct ModuleExports {
    pub own: Vec<Export>,
    reexports: Vec<ReExport>,
}

/// `export { a as b } from "spec"` (`names`: `(a, b)`) or `export * from "spec"` (`all`).
#[derive(Debug)]
struct ReExport {
    spec: String,
    names: Vec<(String, String)>,
    all: bool,
}

/// Parse the source file at `path` (`.ts` without JSX, else Velt); `None` if it cannot be read.
pub fn parse_file(path: &Path) -> Option<(SourceMap, FileId, ast::Module)> {
    let src = std::fs::read_to_string(path).ok()?;
    let mut sm = SourceMap::new();
    let file = sm.add(path, src);
    let text = &sm.get(file).src;
    let parsed = std::panic::catch_unwind(|| {
        if vpm::sources::is_plain_ts(path) {
            velt_syntax::parse_ts_file(file, text).0
        } else {
            velt_syntax::parse_file(file, text).0
        }
    });
    let ast = parsed.ok()?;
    Some((sm, file, ast))
}

/// The exports of a parsed file.
pub fn of_parsed(sm: SourceMap, file: FileId, ast: ast::Module) -> ModuleExports {
    let reexports = ast
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ast::ItemKind::Import(i) if item.exported && !i.from.is_empty() => Some(ReExport {
                spec: i.from.clone(),
                names: i
                    .names
                    .iter()
                    .map(|n| {
                        let exported = n.alias.as_ref().unwrap_or(&n.name);
                        (n.name.name.clone(), exported.name.clone())
                    })
                    .collect(),
                all: i.all,
            }),
            _ => None,
        })
        .collect();
    let module = SourceModule {
        path: "module".into(),
        is_std: false,
        file,
        ast,
        imports: vec![],
        jsx_runtime: None,
    };
    let analysis = Analysis {
        sm,
        modules: vec![module],
        root: 0,
        diagnostics: vec![],
        ide: None,
        ts_compat: vec![],
    };
    // Without loaded imports, the module's items are its own (re-exports resolve nothing).
    let own = index::module_items(&analysis, 0, true)
        .iter()
        .map(|d| of_decl(&analysis, d))
        .collect();
    ModuleExports { own, reexports }
}

/// The export a declaration of `analysis` makes.
pub fn of_decl(analysis: &Analysis, d: &Decl) -> Export {
    Export {
        name: d.name.clone(),
        kind: completion::kind(d),
        detail: signature::decl(analysis, d),
        is_type: d.item().is_some_and(declares_type),
    }
}

/// Whether `item` declares a type.
pub fn declares_type(item: &ast::Item) -> bool {
    matches!(
        item.kind,
        ast::ItemKind::Class(_)
            | ast::ItemKind::Struct(_)
            | ast::ItemKind::Interface(_)
            | ast::ItemKind::Enum(_)
            | ast::ItemKind::TypeAlias(_)
    )
}

/// Parsed files by path, with their modification time.
#[derive(Default)]
pub struct ExportCache {
    files: HashMap<PathBuf, (Option<SystemTime>, Arc<ModuleExports>)>,
}

impl ExportCache {
    /// The exports of the file at `path` (empty if it cannot be read), parsed again only when it
    /// changed on disk.
    fn file(&mut self, path: &Path) -> Arc<ModuleExports> {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some((when, exports)) = self.files.get(path) {
            if when.is_some() && *when == modified {
                return exports.clone();
            }
        }
        let exports = Arc::new(
            parse_file(path)
                .map(|(sm, file, ast)| of_parsed(sm, file, ast))
                .unwrap_or_default(),
        );
        self.files
            .insert(path.to_path_buf(), (modified, exports.clone()));
        exports
    }

    /// Everything the module at `path` exports, re-exports followed (`velt:` specifiers resolve
    /// below `std_root`).
    pub fn exports(&mut self, path: &Path, std_root: Option<&Path>) -> Vec<Export> {
        self.exports_at(path, std_root, 0)
    }

    fn exports_at(&mut self, path: &Path, std_root: Option<&Path>, depth: usize) -> Vec<Export> {
        let module = self.file(path);
        let mut out = module.own.clone();
        if depth >= MAX_REEXPORT_DEPTH {
            return out;
        }
        for re in &module.reexports {
            let target = resolve(&re.spec, path, std_root);
            let found = target.map_or_else(Vec::new, |t| self.exports_at(&t, std_root, depth + 1));
            if re.all {
                out.extend(found);
                continue;
            }
            for (original, exported) in &re.names {
                let mut export = found
                    .iter()
                    .find(|e| e.name == *original)
                    .cloned()
                    .unwrap_or_else(|| Export {
                        name: original.clone(),
                        kind: CompletionItemKind::VARIABLE,
                        detail: String::new(),
                        is_type: false,
                    });
                export.name = exported.clone();
                out.push(export);
            }
        }
        out
    }
}

/// The file a `velt:` or relative specifier of the file `importer` names, if it exists.
pub fn resolve(spec: &str, importer: &Path, std_root: Option<&Path>) -> Option<PathBuf> {
    if let Some(rel) = spec.strip_prefix("velt:") {
        let root = std_root?;
        if rel
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        {
            return None;
        }
        return [
            root.join(format!("{rel}.vlt")),
            root.join(rel).join("index.vlt"),
        ]
        .into_iter()
        .find(|p| p.is_file());
    }
    if spec.starts_with("./") || spec.starts_with("../") {
        let base = vpm::relpath::normalize(&importer.parent()?.join(spec));
        return relative_candidates(&base).into_iter().find(|p| p.is_file());
    }
    None
}

/// The files a relative specifier naming `base` may be: `base` itself when it has a source
/// extension, else `base.vlt`, `base.ts`, `base.tsx`, then the folder module `base/index.*`.
fn relative_candidates(base: &Path) -> Vec<PathBuf> {
    let Some(name) = base.to_str() else {
        return vec![];
    };
    if vpm::sources::is_source_name(name) {
        return vec![base.to_path_buf()];
    }
    let index = base.join("index");
    let index = index.to_str().unwrap_or(name);
    vpm::sources::source_files(name)
        .into_iter()
        .chain(vpm::sources::source_files(index))
        .collect()
}
