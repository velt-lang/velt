//! The symbols of the source files under the workspace folders (`.vlt` files, and `.ts` and
//! `.tsx` files in a package's `src/` and `tests/`: [`collect_files`]), for
//! workspace symbol queries: each file is parsed once and its symbols kept with its modification
//! time. When the client reports file changes (`workspace/didChangeWatchedFiles`, registered at
//! startup when the client supports it), the index follows the events and a query reads nothing
//! from disk; otherwise each query lists the folders again and reparses only the files whose
//! modification time changed. The files' exports are kept too, for auto-import.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lsp_types::WorkspaceSymbol;

use crate::imports::exports::{self, Export};
use crate::manifest::is_manifest;
use crate::workspace_symbols::{
    collect_files, file_symbols, indexes, Search, SourceFile, SKIPPED_DIRS,
};

/// One indexed file.
struct Indexed {
    modified: Option<SystemTime>,
    symbols: Vec<WorkspaceSymbol>,
    /// What it exports (for auto-import).
    exports: Vec<Export>,
    /// The root of the package it belongs to.
    package: Option<PathBuf>,
}

/// The index of the workspace folders' files.
#[derive(Default)]
pub struct DiskIndex {
    files: BTreeMap<PathBuf, Indexed>,
    /// Whether the folders were listed at least once.
    scanned: bool,
    /// Whether the client reports file changes (then the index needs no rescans).
    pub watched: bool,
}

impl DiskIndex {
    /// Add the matching symbols of the indexed files under `roots` to `search` (files the
    /// search already has, such as open documents, are skipped by it).
    pub fn search(&mut self, roots: &[PathBuf], search: &mut Search) {
        if !self.scanned || !self.watched {
            self.rescan(roots);
        }
        for (path, file) in &self.files {
            if search.is_full() {
                return;
            }
            search.add_symbols(path, &file.symbols);
        }
    }

    /// The exports of the indexed files the document at `doc` may import by a relative path:
    /// those of its package (not of a package nested in it), or, outside a package, those of its
    /// workspace folder outside packages.
    pub fn exports(&mut self, roots: &[PathBuf], doc: &Path) -> Vec<(&Path, &[Export])> {
        if !self.scanned || !self.watched {
            self.rescan(roots);
        }
        let package = doc.parent().and_then(vpm::manifest::find_package_root);
        let root = roots.iter().find(|r| doc.starts_with(r));
        self.files
            .iter()
            .filter(|(path, file)| {
                !file.exports.is_empty()
                    && file.package == package
                    && (package.is_some() || root.is_some_and(|r| path.starts_with(r)))
            })
            .map(|(path, file)| (path.as_path(), file.exports.as_slice()))
            .collect()
    }

    /// List the folders: forget files that are gone, (re)parse new and modified ones.
    fn rescan(&mut self, roots: &[PathBuf]) {
        let mut paths = vec![];
        for root in roots {
            collect_files(root, &mut paths);
        }
        let present: HashSet<&PathBuf> = paths.iter().collect();
        self.files.retain(|p, _| present.contains(p));
        for path in &paths {
            let modified = modified(path);
            let fresh = self
                .files
                .get(path)
                .is_some_and(|f| modified.is_some() && f.modified == modified);
            if !fresh {
                self.files.insert(path.clone(), index_file(path, modified));
            }
        }
        self.scanned = true;
    }

    /// The client reports that the file or directory at `path` was created, changed
    /// (`deleted == false`) or deleted. Paths outside `roots` or in skipped directories are
    /// ignored.
    pub fn changed(&mut self, roots: &[PathBuf], path: &Path, deleted: bool) {
        if !self.scanned || !is_indexed(roots, path) {
            return;
        }
        // A deleted or renamed directory: everything under it is gone.
        self.files
            .retain(|p, _| p == path || !p.starts_with(path) || p.exists());
        if deleted || !path.exists() {
            self.files.remove(path);
        } else if path.is_dir() {
            // A created or renamed directory: its files (watchers report only the directory).
            let mut paths = vec![];
            collect_files(path, &mut paths);
            for p in paths {
                let modified = modified(&p);
                self.files.insert(p.clone(), index_file(&p, modified));
            }
        } else if vpm::sources::is_source_file(path) && !is_manifest(path) && indexes(path) {
            self.files
                .insert(path.to_path_buf(), index_file(path, modified(path)));
        }
    }
}

/// Is `path` under one of `roots`, outside hidden and skipped directories (a directory's own
/// name included)?
fn is_indexed(roots: &[PathBuf], path: &Path) -> bool {
    roots.iter().any(|root| {
        path.strip_prefix(root).is_ok_and(|rel| {
            let dirs = if path.is_dir() {
                Some(rel)
            } else {
                rel.parent()
            };
            let mut dirs = dirs.into_iter().flat_map(Path::components);
            !dirs.any(|c| {
                let name = c.as_os_str().to_str().unwrap_or("");
                name.starts_with('.') || SKIPPED_DIRS.contains(&name)
            })
        })
    })
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Parse the file at `path` (nothing if it cannot be read) for its symbols and exports.
fn index_file(path: &Path, modified: Option<SystemTime>) -> Indexed {
    let package = path.parent().and_then(vpm::manifest::find_package_root);
    let Some((sm, file, ast)) = exports::parse_file(path) else {
        return Indexed {
            modified,
            symbols: vec![],
            exports: vec![],
            package,
        };
    };
    let symbols = file_symbols(&SourceFile {
        path,
        text: &sm.get(file).src,
        ast: &ast,
    });
    Indexed {
        modified,
        symbols,
        exports: exports::of_parsed(sm, file, ast).own,
        package,
    }
}
