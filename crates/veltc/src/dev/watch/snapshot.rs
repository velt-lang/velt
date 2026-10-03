//! What the watched files and directories looked like when a build started: the watcher's last
//! observed state, copied without touching the file system.
//!
//! A build reads its sources some time after it starts, and files saved meanwhile are reported
//! (or polled) while it runs. Comparing the files after the build with this snapshot, instead of
//! with what they look like once the build is done, catches every save that happened during the
//! build, also in files the build read for the first time (a module that was still being
//! written when the build read it): a save after the snapshot leads to another build.
//!
//! Files are compared by modification time and length, so two saves of the same length within
//! one tick of the file system's clock still look the same. Files the snapshot doesn't cover (in a
//! directory nothing was watched in, or there since before the watcher looked) count as saved
//! during the build when they were modified less than [`SLACK`] before it started: file times come
//! from a coarser clock than the build's start time. Before the first build the watcher is seeded
//! with the program's directories ([`Watcher::seed`]), so the usual case is covered exactly.

use std::cell::OnceCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{canonical, sources_in, stamp, Stamp, Watcher};

/// How long before a build's start a file the snapshot doesn't cover may have been modified and
/// still count as saved during the build (file system clocks are coarse: about 16 ms on
/// Windows, seconds on FAT). At worst this costs one extra build.
pub const SLACK: Duration = Duration::from_secs(2);

/// The watched files' and directories' state at the start of a build.
pub struct Snapshot {
    taken: SystemTime,
    /// Path (as the watcher spells it) → stamp, for the watched files and the `.vlt` files that
    /// appeared in watched directories since the last build.
    stamps: HashMap<PathBuf, Option<Stamp>>,
    /// Directory (as the watcher spells it) → its `.vlt` files.
    listings: HashMap<PathBuf, BTreeSet<PathBuf>>,
    /// Canonical spelling → the watcher's spelling, built only when a path the build reports is
    /// spelled differently.
    canonical: OnceCell<HashMap<PathBuf, PathBuf>>,
}

impl Snapshot {
    /// `path`'s stamp when the snapshot was taken, if the snapshot covers it.
    pub(super) fn stamp_of(&self, path: &Path) -> Option<Option<Stamp>> {
        match self.stamps.get(path) {
            Some(s) => Some(*s),
            None => self.stamps.get(self.spelling(path)?).copied(),
        }
    }

    /// The `.vlt` files `dir` held when the snapshot was taken (spelled under `dir`).
    pub(super) fn listing(&self, dir: &Path) -> Option<BTreeSet<PathBuf>> {
        let (known, files) = match self.listings.get_key_value(dir) {
            Some(entry) => entry,
            None => self.listings.get_key_value(self.spelling(dir)?)?,
        };
        if known == dir {
            return Some(files.clone());
        }
        let names = files.iter().filter_map(|f| f.file_name());
        Some(names.map(|n| dir.join(n)).collect())
    }

    /// Whether a file the snapshot doesn't cover, with stamp `s`, may have been saved after the
    /// build started.
    pub(super) fn maybe_saved_since(&self, s: Option<Stamp>) -> bool {
        let since = self.taken.checked_sub(SLACK).unwrap_or(self.taken);
        s.is_some_and(|(modified, _)| modified > since)
    }

    /// The watcher's spelling of `path`, when it spells it differently.
    fn spelling(&self, path: &Path) -> Option<&PathBuf> {
        let index = self.canonical.get_or_init(|| {
            let paths = self.stamps.keys().chain(self.listings.keys());
            paths
                .filter_map(|p| Some((canonical(p)?, p.clone())))
                .collect()
        });
        index.get(&canonical(path)?)
    }
}

impl Watcher {
    /// Take a snapshot before a build starts; pass it to [`Watcher::set`] or [`Watcher::add`]
    /// with the files the build read.
    pub fn snapshot(&self) -> Snapshot {
        // The state as last observed, no file system calls: a save the watcher hasn't seen yet
        // only makes the post-build comparison find one more change (one more build).
        Snapshot {
            taken: SystemTime::now(),
            stamps: self
                .appeared
                .iter()
                .chain(&self.files)
                .map(|(p, s)| (p.clone(), *s))
                .collect(),
            listings: self
                .dirs
                .iter()
                .map(|(d, l)| (d.clone(), l.clone()))
                .collect(),
            canonical: OnceCell::new(),
        }
    }

    /// Before the first build: also cover `dirs` (the program's directory), so saves there
    /// during the first build are caught exactly like later ones.
    pub fn seed(&mut self, dirs: impl IntoIterator<Item = PathBuf>) {
        for dir in dirs {
            if dir.is_dir() && !self.dirs.contains_key(&dir) {
                let sources = sources_in(&dir);
                for file in &sources {
                    self.files.insert(file.clone(), stamp(file));
                }
                self.dirs.insert(dir, sources);
            }
        }
    }
}
