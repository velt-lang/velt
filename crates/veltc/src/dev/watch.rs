//! Change detection for `velt dev` / `velt test --watch`: the exact files the last build read
//! (plus the manifest and lockfile), not a directory tree, so edits to std or path dependencies
//! count and unrelated files don't. A `.vlt` file that appears next to one of them counts too:
//! it may be the module a failed build was missing, which no build has read yet.
//!
//! The operating system reports changes in the watched files' directories (`notify`: inotify,
//! FSEvents, ReadDirectoryChangesW); each report is checked against the file's modification
//! time and length, so a report that changed nothing is ignored. Where notifications are not
//! available (or `VELT_DEV_POLL=1`, e.g. on network file systems that don't deliver them), the
//! files' stamps and the directories' `.vlt` listings are polled every [`POLL`] instead.
//! A change is reported once the files have been quiet for [`SETTLE`], so an editor's
//! write-rename-touch sequence triggers one rebuild.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant, SystemTime};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

/// Interval between checks.
pub const POLL: Duration = Duration::from_millis(10);
/// Quiet time after the last change before it is reported.
pub const SETTLE: Duration = Duration::from_millis(30);
/// Set to `1` to poll instead of using the operating system's change notifications.
pub const POLL_ENV: &str = "VELT_DEV_POLL";

/// The watched files and the state each had when last looked at.
pub struct Watcher {
    files: BTreeMap<PathBuf, Option<Stamp>>,
    /// The watched files' directories and the `.vlt` files each held when last looked at
    /// (polling only; with notifications the operating system reports new files).
    dirs: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
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

/// What identifies a file version: modification time and length (length catches rewrites
/// within the filesystem's timestamp granularity).
type Stamp = (SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn is_source(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "vlt")
}

/// The `.vlt` files in `dir`.
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

    /// Watch exactly `paths` from now on, as read by a build that started at `built_from`:
    /// a file modified after that (saved during the build) counts as changed right away.
    pub fn set(&mut self, paths: impl IntoIterator<Item = PathBuf>, built_from: SystemTime) {
        self.files = paths
            .into_iter()
            .map(|p| {
                let s = stamp(&p);
                (p, s)
            })
            .collect();
        self.dirs.clear();
        self.watch_dirs();
        let now = Instant::now();
        let newer = self
            .files
            .values()
            .any(|s| s.is_some_and(|(modified, _)| modified > built_from));
        self.dirty_since = newer.then_some(now);
        self.last_change = newer.then_some(now);
    }

    /// Also watch `paths` (after a failed build: the files it read may differ from the last
    /// good build's, e.g. a newly added import).
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for p in paths {
            self.files.entry(p).or_insert_with_key(|p| stamp(p));
        }
        self.watch_dirs();
    }

    /// Start watching the directories of the watched files (and stop watching others). A
    /// directory the notifier cannot watch switches the watcher to polling.
    fn watch_dirs(&mut self) {
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
                self.dirs.insert(dir.clone(), sources_in(dir));
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
    /// `.vlt` file in a watched directory.
    fn check_paths(&mut self, paths: Vec<PathBuf>) -> bool {
        let mut changed = false;
        for path in paths {
            let path = self.key(path);
            if let Some(old) = self.files.get_mut(&path) {
                let new = stamp(&path);
                if new != *old {
                    *old = new;
                    changed = true;
                }
            } else if is_source(&path) && path.exists() {
                let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
                let dir = self.key(dir);
                if let (Some(known), Some(name)) = (self.dirs.get_mut(&dir), path.file_name()) {
                    changed |= known.insert(dir.join(name));
                }
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

    /// Polling: every watched file's stamp and every watched directory's `.vlt` files.
    fn check_all(&mut self) -> bool {
        let mut changed = false;
        for (path, old) in self.files.iter_mut() {
            let new = stamp(path);
            if new != *old {
                *old = new;
                changed = true;
            }
        }
        for (dir, known) in self.dirs.iter_mut() {
            let now = sources_in(dir);
            if now.iter().any(|f| !known.contains(f)) {
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

    /// The paths reported since the last call (reads are not changes); `Err` when the
    /// notifier reported an error and can no longer be trusted.
    fn changed_paths(&self) -> Result<Vec<PathBuf>, ()> {
        let mut paths = vec![];
        for event in self.events.try_iter() {
            let event = event.map_err(|_| ())?;
            if !matches!(event.kind, EventKind::Access(_)) {
                paths.extend(event.paths);
            }
        }
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Poll until a change is reported (or `limit` passes).
    fn wait(w: &mut Watcher, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if w.poll().is_some() {
                return true;
            }
            std::thread::sleep(POLL);
        }
        false
    }

    /// Edits, files that appear, saves during the build and new `.vlt` files next to watched
    /// ones, each reported once after it settles.
    fn reports_changes(notify: bool) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.vlt");
        std::fs::write(&file, "a").unwrap();
        let mut w = Watcher::new(notify);
        assert_eq!(w.notifies(), notify);
        w.set(
            [file.clone(), dir.path().join("missing.vlt")],
            SystemTime::now(),
        );
        assert!(!wait(&mut w, SETTLE * 3), "nothing changed");
        std::fs::write(&file, "bb").unwrap();
        assert!(w.poll().is_none(), "not settled yet");
        assert!(wait(&mut w, Duration::from_secs(5)));
        assert!(!wait(&mut w, SETTLE * 3), "reported once");
        // A file that appears counts as a change.
        std::fs::write(dir.path().join("missing.vlt"), "x").unwrap();
        assert!(wait(&mut w, Duration::from_secs(5)));
        // So does a new module nothing has read yet; other new files don't.
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        assert!(!wait(&mut w, SETTLE * 5), "not a source file");
        std::fs::write(dir.path().join("added.vlt"), "x").unwrap();
        assert!(wait(&mut w, Duration::from_secs(5)));
        // Saved while the build that read it was running.
        let before = SystemTime::now() - Duration::from_secs(5);
        w.set([file.clone()], before);
        assert!(wait(&mut w, Duration::from_secs(1)));
    }

    #[test]
    fn reports_changes_by_polling() {
        reports_changes(false);
    }

    #[test]
    fn reports_changes_from_notifications() {
        if Watcher::new(true).notifies() {
            reports_changes(true);
        }
    }
}
