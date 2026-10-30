//! Test results kept for programs that can't change their minds (#1855 step 3, the #1856
//! review's rule): when a test file's program is the same as when its tests passed, and the
//! compiler found it reaches nothing outside itself (no file system, network, clock, environment
//! or random numbers: `driver::outside`, recorded in the build stamp), its tests pass again, so
//! the runner prints the kept report instead of running them. A program that does reach out runs
//! every time, and its file says what it reaches.
//!
//! `<harness>.test-results.json` beside each harness holds, under a key over the harness's bytes,
//! `--filter`/`--exact` and the leak check (`VELT_RC_STATS`), the reports of its files that passed. `--no-cache`, and
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
    /// The harness's build stamp as it was before the runner started: what the member stamp is
    /// made of, so another run rebuilding the harness meanwhile doesn't lend it its sources.
    build_stamp: Option<String>,
}

impl Harness {
    /// What is known of the harness `exe`, run with `filter`/`exact`; `enabled` is false for
    /// `--no-cache`.
    pub fn of(exe: &Path, filter: Option<&str>, exact: bool, enabled: bool) -> Harness {
        // Before the program is read: the stamp's `link` entry then catches a relink after.
        let build_stamp = std::fs::read_to_string(stamp::path(exe)).ok();
        let record = (enabled && stamp::enabled())
            .then(|| stamp::outside(exe))
            .flatten();
        let key = match (&record, std::fs::read(exe)) {
            (Some(_), Ok(bytes)) => {
                // `VELT_RC_STATS=1` makes the runner fail a file that leaks: a result kept
                // without that check doesn't stand for one with it.
                let leaks = std::env::var("VELT_RC_STATS").unwrap_or_default();
                let hash = vpm::probe::content_hash(&bytes);
                format!("{hash} {filter:?} {exact} {leaks:?}")
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
            build_stamp,
        }
    }

    /// The harness's build stamp, as it was when the run began.
    pub fn build_stamp(&self) -> Option<&str> {
        self.build_stamp.as_deref()
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

/// The reports kept at `path` under `key`: those shaped as the runner's `Ran` of a file whose
/// tests all passed. Anything else is dropped, so a broken file means "not kept", never a run
/// that fails on what it was handed.
fn read(path: &Path, key: &str) -> Option<BTreeMap<String, Value>> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    (value.get("key")?.as_str()? == key).then_some(())?;
    let files = value.get("files")?.as_object()?;
    Some(
        files
            .iter()
            .filter(|(_, v)| is_passed_report(v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

/// Whether `v` is a `Ran` (`index`, `lines`, `passed`, `failed`) with no failures.
fn is_passed_report(v: &Value) -> bool {
    let lines = v["lines"]
        .as_array()
        .is_some_and(|l| l.iter().all(Value::is_string));
    lines && v["index"].is_u64() && v["passed"].is_u64() && v["failed"].as_u64() == Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_reports_are_read_back_under_their_key_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("h.test-results.json");
        let good = r#"{"index": 0, "lines": ["ok t"], "passed": 2, "failed": 0}"#;
        std::fs::write(
            &path,
            format!(r#"{{"key": "k1", "files": {{"0": {good}}}}}"#),
        )
        .unwrap();
        let reports = read(&path, "k1").unwrap();
        assert_eq!(reports["0"]["passed"], 2);
        assert!(read(&path, "k2").is_none());
        // Entries of the wrong shape, or with failures, are not kept: the tests just run.
        let bad = [
            r#"{"index": 0, "lines": 5, "passed": "x", "failed": 0}"#,
            r#"{"index": 0, "lines": [1], "passed": 1, "failed": 0}"#,
            r#"{"index": 0, "lines": [], "passed": 1, "failed": 1}"#,
            r#"{"lines": [], "passed": 1, "failed": 0}"#,
        ];
        for entry in bad {
            std::fs::write(
                &path,
                format!(r#"{{"key": "k1", "files": {{"0": {entry}}}}}"#),
            )
            .unwrap();
            assert!(read(&path, "k1").unwrap().is_empty(), "{entry}");
        }
        assert_eq!(
            results_path(Path::new("/t/h_1")),
            Path::new("/t/h_1.test-results.json")
        );
    }
}
