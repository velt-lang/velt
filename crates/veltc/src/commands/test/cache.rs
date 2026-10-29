//! Test results kept for programs that can't change their minds (#1855 step 3, the #1856
//! review's rule): when a test file's program is the same as when its tests passed, and the
//! compiler found it reaches nothing outside itself (no file system, network, clock, environment
//! or random numbers: `driver::outside`, recorded in the build stamp), its tests pass again, so
//! the runner prints the kept report instead of running them. A program that does reach out runs
//! every time, and its file says what it reaches.
//!
//! `<harness>.test-results.json` beside each harness holds, under a key over the harness's bytes
//! and `--filter`/`--exact`, the reports of its files that passed. `--no-cache`, and
//! `VELT_BUILD_STAMP=off` (no stamp knows what a program reaches), run everything.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::driver::{outside, stamp};

/// What the cache knows of one harness program.
pub(super) struct Harness {
    /// What the program reaches, as its build stamp records it (`driver::outside`); `None`
    /// without a stamp, or with caching off: then nothing is kept.
    record: Option<String>,
    key: String,
    /// The kept reports by the file's index in the harness.
    reports: BTreeMap<String, Value>,
}

impl Harness {
    /// What is known of the harness `exe`, run with `filter`/`exact`; `enabled` is false for
    /// `--no-cache`.
    pub fn of(exe: &Path, filter: Option<&str>, exact: bool, enabled: bool) -> Harness {
        let record = (enabled && stamp::enabled())
            .then(|| stamp::outside(exe))
            .flatten();
        let key = match (&record, std::fs::read(exe)) {
            (Some(_), Ok(bytes)) => {
                format!("{} {filter:?} {exact}", vpm::probe::content_hash(&bytes))
            }
            _ => String::new(),
        };
        let reports = (!key.is_empty())
            .then(|| read(&results_path(exe), &key))
            .flatten()
            .unwrap_or_default();
        Harness {
            record,
            key,
            reports,
        }
    }

    /// What the tests of the file at `index` reach, for the runner to show (`the network`).
    pub fn uncacheable(&self, index: usize) -> Option<String> {
        outside::of_file(self.record.as_deref()?, index)
    }

    /// Whether the report of the file at `index` may be kept and used.
    pub fn keeps(&self, index: usize) -> bool {
        !self.key.is_empty() && self.uncacheable(index).is_none()
    }

    /// Whether the report of any file of the harness may be kept.
    pub fn keeps_any(&self) -> bool {
        !self.key.is_empty()
    }

    /// The kept report of the file at `index`, if its tests passed last time.
    pub fn cached(&self, index: usize) -> Option<&Value> {
        self.reports
            .get(&index.to_string())
            .filter(|_| self.keeps(index))
    }

    /// Keep the report `ran` (the runner's `Ran`, without failures) of the file at `index`.
    pub fn keep(&mut self, index: usize, ran: Value) {
        if self.keeps(index) {
            self.reports.insert(index.to_string(), ran);
        }
    }

    /// Write what is kept beside `exe` (best effort: without it the tests just run again).
    pub fn save(&self, exe: &Path) {
        if self.key.is_empty() {
            return;
        }
        let files: Map<String, Value> = self.reports.clone().into_iter().collect();
        let text = json!({ "key": self.key, "files": files }).to_string();
        let path = results_path(exe);
        let mut tmp = path.clone().into_os_string();
        tmp.push(format!(".{}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// `<exe>.test-results.json`.
fn results_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".test-results.json");
    exe.with_file_name(name)
}

/// The reports kept at `path` under `key`.
fn read(path: &Path, key: &str) -> Option<BTreeMap<String, Value>> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    (value.get("key")?.as_str()? == key).then_some(())?;
    let files = value.get("files")?.as_object()?;
    Some(files.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_reports_are_read_back_under_their_key_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("h.test-results.json");
        std::fs::write(&path, r#"{"key": "k1", "files": {"0": {"passed": 2}}}"#).unwrap();
        let reports = read(&path, "k1").unwrap();
        assert_eq!(reports["0"]["passed"], 2);
        assert!(read(&path, "k2").is_none());
        assert_eq!(
            results_path(Path::new("/t/h_1")),
            Path::new("/t/h_1.test-results.json")
        );
    }
}
