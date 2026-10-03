//! Change detection for `velt dev` / `velt test --watch`: the exact files the last build read
//! (plus the manifest and lockfile), not a directory tree, so edits to std or path dependencies
//! count and unrelated files don't. A source file (`.vlt`, `.ts`, `.tsx`) that appears next to
//! one of them counts too: it may be the module a failed build was missing, which no build has
//! read yet.
//!
//! The operating system reports changes in the watched files' directories (`notify`: inotify,
//! FSEvents, ReadDirectoryChangesW); each report is checked against the file's modification
//! time and length, so a report that changed nothing is ignored. Where notifications are not
//! available (or `VELT_DEV_POLL=1`, e.g. on network file systems that don't deliver them), the
//! files' stamps and the directories' source file listings are polled every [`POLL`] instead.
//! A change is reported once the files have been quiet for [`SETTLE`], so an editor's
//! write-rename-touch sequence triggers one rebuild.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant, SystemTime};

use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

mod snapshot;
pub use snapshot::Snapshot;

/// Interval between checks.
pub const POLL: Duration = Duration::from_millis(10);
/// Quiet time after the last change before it is reported.
pub const SETTLE: Duration = Duration::from_millis(30);
/// Set to `1` to poll instead of using the operating system's change notifications.
pub const POLL_ENV: &str = "VELT_DEV_POLL";

/// The watched files and the state each had when last looked at.
pub struct Watcher {
    files: BTreeMap<PathBuf, Option<Stamp>>,
    /// The watched files' directories and the source files each held when last looked at
    /// (polling only; with notifications the operating system reports new files).
    dirs: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    /// Source files that appeared in a watched directory since the last build, with their stamp
    /// when they appeared (a build that reads one for the first time compares with it).
    appeared: HashMap<PathBuf, Option<Stamp>>,
    /// Change notifications, while they work.
    notifier: Option<Notifier>,
    /// Canonical spellings of the watched files and directories → their keys above
    /// (notifications may name them differently: absolute, through symlinks resolved).
    aliases: HashMap<PathBuf, PathBuf>,
    /// When a change was first seen and not yet reported.
    dirty_since: Option<Instant>,
    /// Last time any file changed (for the settle delay).
    last_change: Option<Instant>,
}

/// The operating system's change notifications for a set of directories.
struct Notifier {
    watcher: RecommendedWatcher,
    events: Receiver<notify::Result<Event>>,
    watched: BTreeSet<PathBuf>,
}

/// What a notification says happened to a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// Created, or renamed to this name.
    Appeared,
    /// Deleted, or renamed away from this name.
    Gone,
    /// Anything else (look at the file).
    Changed,
}

/// What identifies a file version: modification time and length (length catches rewrites
/// within the filesystem's timestamp granularity).
type Stamp = (SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn is_source(path: &Path) -> bool {
    vpm::sources::is_source_file(path)
}

/// The source files in `dir`.
fn sources_in(dir: &Path) -> BTreeSet<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| is_source(p))
                .collect()
        })
        .unwrap_or_default()
}

/// `path` with its directory resolved (the file itself may no longer exist).
fn canonical(path: &Path) -> Option<PathBuf> {
    if let Ok(full) = std::fs::canonicalize(path) {
        return Some(full);
    }
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    let dir = std::fs::canonicalize(dir.unwrap_or(Path::new("."))).ok()?;
    Some(dir.join(path.file_name()?))
}

impl Default for Watcher {
    /// Notifications unless they are unavailable or [`POLL_ENV`] asks for polling.
    fn default() -> Watcher {
        let poll = std::env::var_os(POLL_ENV).is_some_and(|v| v == "1");
        Watcher::new(!poll)
    }
}

impl Watcher {
    /// A watcher that uses change notifications when `notify` is set and they are available.
    pub fn new(notify: bool) -> Watcher {
        let notifier = notify.then(Notifier::new).flatten();
        Watcher {
            files: BTreeMap::new(),
            dirs: BTreeMap::new(),
            appeared: HashMap::new(),
            notifier,
            aliases: HashMap::new(),
            dirty_since: None,
            last_change: None,
        }
    }

    /// Whether changes come from operating system notifications (else polling).
    #[cfg(test)]
    pub fn notifies(&self) -> bool {
        self.notifier.is_some()
    }

    /// Watch exactly `paths` from now on, as read by a build that started at `snap`: a file
    /// saved since (during the build) counts as changed right away.
    pub fn set(&mut self, paths: impl IntoIterator<Item = PathBuf>, snap: &Snapshot) {
        self.files.clear();
        self.dirs.clear();
        self.appeared.clear();
        self.dirty_since = None;
        self.last_change = None;
        self.add(paths, snap);
    }

    /// Also watch `paths`, as read by a build that started at `snap` (after a failed build:
    /// the files it read may differ from the last good build's, e.g. a newly added import).
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>, snap: &Snapshot) {
        let mut newer = false;
        for p in paths {
            if self.files.contains_key(&p) {
                continue;
            }
            let s = match snap.stamp_of(&p) {
                Some(s) => s,
                // Not in a directory watched before the build: modified around its start
                // may mean saved during the build.
                None => {
                    let s = stamp(&p);
                    newer |= snap.maybe_saved_since(s);
                    s
                }
            };
            self.files.insert(p, s);
        }
        newer |= self.watch_dirs(snap);
        // Anything saved since the snapshot, including files the build read for the first
        // time and modules that appeared while it ran.
        if self.check_all() || newer {
            let now = Instant::now();
            self.dirty_since.get_or_insert(now);
            self.last_change = Some(now);
        }
    }

    /// Start watching the directories of the watched files (and stop watching others), with
    /// the source files they held at `snap`. A directory the notifier cannot watch switches the
    /// watcher to polling. Whether a directory the snapshot doesn't cover holds a source file
    /// that may have been saved during the build.
    fn watch_dirs(&mut self, snap: &Snapshot) -> bool {
        let mut newer = false;
        let wanted: BTreeSet<PathBuf> = self
            .files
            .keys()
            .filter_map(|f| f.parent())
            .map(|d| {
                if d.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    d.to_path_buf()
                }
            })
            .collect();
        for dir in &wanted {
            if !self.dirs.contains_key(dir) {
                let known = snap.listing(dir).unwrap_or_else(|| {
                    let now = sources_in(dir);
                    newer |= now.iter().any(|f| snap.maybe_saved_since(stamp(f)));
                    now
                });
                self.dirs.insert(dir.clone(), known);
            }
        }
        self.dirs.retain(|d, _| wanted.contains(d));
        self.aliases = self
            .files
            .keys()
            .chain(self.dirs.keys())
            .filter_map(|p| Some((canonical(p)?, p.clone())))
            .filter(|(canon, p)| canon != p)
            .collect();
        if let Some(notifier) = &mut self.notifier {
            if notifier.watch(&wanted).is_err() {
                self.notifier = None;
            }
        }
        newer
    }

    /// Check once; `Some(first change time)` when changes have settled.
    pub fn poll(&mut self) -> Option<Instant> {
        let changed = match self.notifier.as_ref().map(Notifier::changed_paths) {
            Some(Ok(paths)) => self.check_paths(paths),
            Some(Err(())) => {
                // Notifications failed (e.g. too many watches): poll from now on.
                self.notifier = None;
                self.check_all()
            }
            None => self.check_all(),
        };
        let now = Instant::now();
        if changed {
            self.dirty_since.get_or_insert(now);
            self.last_change = Some(now);
        }
        match (self.dirty_since, self.last_change) {
            (Some(first), Some(last)) if now.duration_since(last) >= SETTLE => {
                self.dirty_since = None;
                Some(first)
            }
            _ => None,
        }
    }

    /// Notifications: whether any of `paths` is a watched file whose stamp changed, or a new
    /// source file in a watched directory.
    /// A file that was deleted and created again (`git stash`, a branch switch) is new again.
    fn check_paths(&mut self, paths: Vec<(Seen, PathBuf)>) -> bool {
        let mut changed = false;
        for (seen, path) in paths {
            let path = self.key(path);
            if let Some(old) = self.files.get_mut(&path) {
                let new = stamp(&path);
                if new != *old {
                    *old = new;
                    changed = true;
                }
                continue;
            }
            if !is_source(&path) {
                continue;
            }
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let dir = self.key(dir);
            let (Some(known), Some(name)) = (self.dirs.get_mut(&dir), path.file_name()) else {
                continue;
            };
            let entry = dir.join(name);
            let present = match seen {
                Seen::Appeared => true,
                Seen::Gone => false,
                Seen::Changed => path.exists(),
            };
            if present {
                let new = known.insert(entry.clone());
                changed |= new;
                // Keep its last state, for a build that reads it for the first time.
                if new || self.appeared.contains_key(&entry) {
                    self.appeared.insert(entry.clone(), stamp(&entry));
                }
            } else {
                known.remove(&entry);
            }
        }
        changed
    }

    /// The watched path that `path` (as a notification names it) is.
    fn key(&self, path: PathBuf) -> PathBuf {
        if self.files.contains_key(&path) || self.dirs.contains_key(&path) {
            return path;
        }
        canonical(&path)
            .and_then(|c| self.aliases.get(&c).cloned())
            .unwrap_or(path)
    }

    /// Polling: every watched file's stamp and every watched directory's source files.
    fn check_all(&mut self) -> bool {
        let mut changed = false;
        // Modules nothing has read yet: their last state, for a build that reads them.
        for (path, last) in self.appeared.iter_mut() {
            *last = stamp(path);
        }
        for (path, old) in self.files.iter_mut() {
            let new = stamp(path);
            if new != *old {
                *old = new;
                changed = true;
            }
        }
        // A watched file that appears was counted above (notifications don't list it).
        let files = &self.files;
        for (dir, known) in self.dirs.iter_mut() {
            let now = sources_in(dir);
            let new = now
                .iter()
                .filter(|f| !known.contains(*f) && !files.contains_key(*f));
            for new in new {
                self.appeared.insert(new.clone(), stamp(new));
                changed = true;
            }
            *known = now;
        }
        changed
    }
}

impl Notifier {
    fn new() -> Option<Notifier> {
        let (tx, events) = channel();
        let watcher = notify::recommended_watcher(tx).ok()?;
        Some(Notifier {
            watcher,
            events,
            watched: BTreeSet::new(),
        })
    }

    /// Watch exactly `dirs`.
    fn watch(&mut self, dirs: &BTreeSet<PathBuf>) -> notify::Result<()> {
        for gone in self.watched.difference(dirs) {
            let _ = self.watcher.unwatch(gone);
        }
        self.watched.retain(|d| dirs.contains(d));
        for dir in dirs {
            if !self.watched.contains(dir) && dir.is_dir() {
                self.watcher.watch(dir, RecursiveMode::NonRecursive)?;
                self.watched.insert(dir.clone());
            }
        }
        Ok(())
    }

    /// The paths reported since the last call, in order, with what happened to them (reads
    /// are not changes); `Err` when the notifier reported an error and can no longer be
    /// trusted.
    fn changed_paths(&self) -> Result<Vec<(Seen, PathBuf)>, ()> {
        let mut paths = vec![];
        for event in self.events.try_iter() {
            let event = event.map_err(|_| ())?;
            let seen = match event.kind {
                EventKind::Access(_) => continue,
                EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                    Seen::Appeared
                }
                EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                    Seen::Gone
                }
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                    // `paths` is [from, to].
                    let mut it = event.paths.into_iter();
                    paths.extend(it.next().map(|from| (Seen::Gone, from)));
                    paths.extend(it.map(|to| (Seen::Appeared, to)));
                    continue;
                }
                _ => Seen::Changed,
            };
            paths.extend(event.paths.into_iter().map(|p| (seen, p)));
        }
        Ok(paths)
    }
}

#[cfg(test)]
mod tests;
