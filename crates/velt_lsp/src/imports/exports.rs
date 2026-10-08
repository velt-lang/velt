//! The exports of modules that are not (or not yet) part of the document's program, read by
//! parsing alone: their names, kinds and signatures, for completion inside `import { … }` and
//! auto-import. Re-exports (`export { a } from "…"`, `export * from "…"`) are followed through
//! the loader's resolution. Parsed files are cached by modification time.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use lsp_types::CompletionItemKind;
use velt_common::{FileId, SourceMap};
use velt_sema::SourceModule;
use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::index::{self, Decl};
use crate::{completion, signature};

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
        docs: Default::default(),
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

/// How long a parsed file is trusted before its modification time is looked at again (changes
/// the editor's file watcher reports are taken at once, through [`ExportCache::forget`]).
const RECHECK: Duration = Duration::from_secs(2);

/// One parsed file.
struct Parsed {
    modified: Option<SystemTime>,
    checked: Instant,
    exports: Arc<ModuleExports>,
}

/// Resolves a specifier of a file to the file it names (the loader's
/// [`crate::ProgramLoader::resolve_module`]).
pub type Resolve<'a> = &'a dyn Fn(&str, &Path) -> Option<PathBuf>;

/// Parsed files by path, and the modules' exports with re-exports followed.
#[derive(Default)]
pub struct ExportCache {
    files: HashMap<PathBuf, Parsed>,
    resolved: HashMap<PathBuf, (Instant, Arc<Vec<Export>>)>,
}

impl ExportCache {
    /// The file at `path` changed on disk: read it again when it is next needed.
    pub fn forget(&mut self, path: &Path) {
        self.files.remove(path);
        // Any module may re-export it.
        self.resolved.clear();
    }

    /// The exports of the file at `path` (empty if it cannot be read), parsed again only when it
    /// changed on disk.
    fn file(&mut self, path: &Path) -> Arc<ModuleExports> {
        if let Some(parsed) = self.files.get_mut(path) {
            if parsed.checked.elapsed() < RECHECK {
                return parsed.exports.clone();
            }
            let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            if modified.is_some() && modified == parsed.modified {
                parsed.checked = Instant::now();
                return parsed.exports.clone();
            }
        }
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let exports = Arc::new(
            parse_file(path)
                .map(|(sm, file, ast)| of_parsed(sm, file, ast))
                .unwrap_or_default(),
        );
        let parsed = Parsed {
            modified,
            checked: Instant::now(),
            exports: exports.clone(),
        };
        self.files.insert(path.to_path_buf(), parsed);
        exports
    }

    /// Everything the module at `path` exports, re-exports followed through `resolve`.
    pub fn exports(&mut self, path: &Path, resolve: Resolve) -> Arc<Vec<Export>> {
        if let Some((checked, exports)) = self.resolved.get(path) {
            if checked.elapsed() < RECHECK {
                return exports.clone();
            }
        }
        let exports = self.collect(path, resolve, &mut HashMap::new(), &mut HashSet::new());
        self.resolved
            .insert(path.to_path_buf(), (Instant::now(), exports.clone()));
        exports
    }

    /// The exports of `path`; `done` holds the modules resolved in this lookup (a module two
    /// re-exports reach is read once), `open` those being resolved (a cycle ends there).
    fn collect(
        &mut self,
        path: &Path,
        resolve: Resolve,
        done: &mut HashMap<PathBuf, Arc<Vec<Export>>>,
        open: &mut HashSet<PathBuf>,
    ) -> Arc<Vec<Export>> {
        if let Some(found) = done.get(path) {
            return found.clone();
        }
        if !open.insert(path.to_path_buf()) {
            return Arc::default();
        }
        let module = self.file(path);
        let mut out = module.own.clone();
        for re in &module.reexports {
            let found = match resolve(&re.spec, path) {
                Some(target) => self.collect(&target, resolve, done, open),
                None => Arc::default(),
            };
            if re.all {
                out.extend(found.iter().cloned());
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
        let out = Arc::new(out);
        open.remove(path);
        done.insert(path.to_path_buf(), out.clone());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-export cycles end, and a module two re-exports reach counts once per path.
    #[test]
    fn re_exports_follow_cycles_and_diamonds_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let files = [
            (
                "a.vlt",
                "export * from \"./b\";\nexport * from \"./c\";\nexport const A: i64 = 1;\n",
            ),
            ("b.vlt", "export * from \"./d\";\nexport * from \"./a\";\n"),
            ("c.vlt", "export { D as E } from \"./d\";\n"),
            ("d.vlt", "export const D: i64 = 1;\n"),
        ];
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        let resolve = |spec: &str, from: &Path| {
            let file = from
                .parent()?
                .join(format!("{}.vlt", spec.strip_prefix("./")?));
            file.is_file().then_some(file)
        };
        let mut cache = ExportCache::default();
        let names: Vec<String> = cache
            .exports(&dir.join("a.vlt"), &resolve)
            .iter()
            .map(|e| e.name.clone())
            .collect();
        assert_eq!(names, ["A", "D", "E"]);
    }
}
