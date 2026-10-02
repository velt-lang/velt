//! What the watched files and directories looked like when a build started.
//!
//! A build reads its sources some time after it starts, and files saved meanwhile are reported
//! (or polled) while it runs. Comparing the files after the build with this snapshot, instead of
//! with what they look like once the build is done, catches every save that happened during the
//! build, also in files the build read for the first time (a module that was still being
//! written when the build read it): a save after the snapshot always triggers another build.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{canonical, sources_in, stamp, Stamp, Watcher};

/// The watched files' and directories' state at the start of a build.
pub struct Snapshot {
    /// When it was taken (for files it doesn't cover: modified later means changed).
    pub(super) taken: SystemTime,
    /// Canonical path → stamp, for the watched files and every `.vlt` file in the watched
    /// directories.
    stamps: HashMap<PathBuf, Option<Stamp>>,
    /// Canonical directory → the names of the `.vlt` files in it.
    listings: HashMap<PathBuf, BTreeSet<OsString>>,
}

impl Snapshot {
    /// `path`'s stamp when the snapshot was taken, if the snapshot covers it.
    pub(super) fn stamp_of(&self, path: &Path) -> Option<Option<Stamp>> {
        self.stamps.get(&canonical(path)?).copied()
    }

    /// The `.vlt` files `dir` held when the snapshot was taken (spelled under `dir`).
    pub(super) fn listing(&self, dir: &Path) -> Option<BTreeSet<PathBuf>> {
        let names = self.listings.get(&canonical(dir)?)?;
        Some(names.iter().map(|n| dir.join(n)).collect())
    }
}

impl Watcher {
    /// Take a snapshot before a build starts; pass it to [`Watcher::set`] or [`Watcher::add`]
    /// with the files the build read.
    pub fn snapshot(&self) -> Snapshot {
        let taken = SystemTime::now();
        let mut stamps = HashMap::new();
        let mut listings = HashMap::new();
        for dir in self.dirs.keys() {
            let sources = sources_in(dir);
            for file in &sources {
                if let Some(c) = canonical(file) {
                    stamps.insert(c, stamp(file));
                }
            }
            let names = sources.iter().filter_map(|f| f.file_name()).map(Into::into);
            if let Some(c) = canonical(dir) {
                listings.insert(c, names.collect());
            }
        }
        for file in self.files.keys() {
            if let Some(c) = canonical(file) {
                stamps.insert(c, stamp(file));
            }
        }
        Snapshot {
            taken,
            stamps,
            listings,
        }
    }
}
