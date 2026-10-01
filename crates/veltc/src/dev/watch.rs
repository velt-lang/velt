//! Change detection for `velt dev` / `velt test --watch`: polls the modification times of the
//! exact files the last build read (plus the manifest and lockfile), not a directory tree, so
//! edits to std or path dependencies count and unrelated files don't.
//!
//! Polling a few dozen files every [`POLL`] costs microseconds and needs no platform watcher.
//! A change is reported once the files have been quiet for [`SETTLE`], so an editor's
//! write-rename-touch sequence triggers one rebuild.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// Interval between checks.
pub const POLL: Duration = Duration::from_millis(10);
/// Quiet time after the last change before it is reported.
pub const SETTLE: Duration = Duration::from_millis(30);

/// The watched files and the state each had when last looked at.
#[derive(Default)]
pub struct Watcher {
    files: BTreeMap<PathBuf, Option<Stamp>>,
    /// When a change was first seen and not yet reported.
    dirty_since: Option<Instant>,
    /// Last time any file changed (for the settle delay).
    last_change: Option<Instant>,
}

/// What identifies a file version: modification time and length (length catches rewrites
/// within the filesystem's timestamp granularity).
type Stamp = (SystemTime, u64);

fn stamp(path: &PathBuf) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

impl Watcher {
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
            self.files.entry(p).or_insert_with_key(stamp);
        }
    }

    /// Check once; `Some(first change time)` when changes have settled.
    pub fn poll(&mut self) -> Option<Instant> {
        let now = Instant::now();
        for (path, old) in self.files.iter_mut() {
            let new = stamp(path);
            if new != *old {
                *old = new;
                self.dirty_since.get_or_insert(now);
                self.last_change = Some(now);
            }
        }
        match (self.dirty_since, self.last_change) {
            (Some(first), Some(last)) if now.duration_since(last) >= SETTLE => {
                self.dirty_since = None;
                Some(first)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_change_once_after_it_settles() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.vlt");
        std::fs::write(&file, "a").unwrap();
        let mut w = Watcher::default();
        w.set(
            [file.clone(), dir.path().join("missing.vlt")],
            SystemTime::now(),
        );
        assert!(w.poll().is_none());
        std::fs::write(&file, "bb").unwrap();
        assert!(w.poll().is_none(), "not settled yet");
        std::thread::sleep(SETTLE + POLL);
        assert!(w.poll().is_some());
        assert!(w.poll().is_none(), "reported once");
        // A file that appears counts as a change.
        std::fs::write(dir.path().join("missing.vlt"), "x").unwrap();
        w.poll();
        std::thread::sleep(SETTLE + POLL);
        assert!(w.poll().is_some());
        // Saved while the build that read it was running.
        let before = SystemTime::now() - Duration::from_secs(5);
        w.set([file.clone()], before);
        std::thread::sleep(SETTLE + POLL);
        assert!(w.poll().is_some());
    }
}
