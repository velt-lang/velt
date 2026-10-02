//! The symbols of the `.vlt` files under the workspace folders, for workspace symbol queries:
//! each file is parsed once and its symbols kept with its modification time. When the client
//! reports file changes (`workspace/didChangeWatchedFiles`, registered at startup when the
//! client supports it), the index follows the events and a query reads nothing from disk;
//! otherwise each query lists the folders again and reparses only the files whose modification
//! time changed.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lsp_types::WorkspaceSymbol;

use crate::workspace_symbols::{collect_files, disk_file_symbols, Search, SKIPPED_DIRS};

/// One indexed file.
struct Indexed {
    modified: Option<SystemTime>,
    symbols: Vec<WorkspaceSymbol>,
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
        } else if path.extension().is_some_and(|e| e == "vlt") {
            self.files
                .insert(path.to_path_buf(), index_file(path, modified(path)));
        }
    }
}

/// Is `path` under one of `roots`, outside hidden and skipped directories?
fn is_indexed(roots: &[PathBuf], path: &Path) -> bool {
    roots.iter().any(|root| {
        path.strip_prefix(root).is_ok_and(|rel| {
            let mut dirs = rel.parent().into_iter().flat_map(Path::components);
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

fn index_file(path: &Path, modified: Option<SystemTime>) -> Indexed {
    Indexed {
        modified,
        symbols: disk_file_symbols(path),
    }
}
