//! Executables of `velt dev --exe`: every version gets its own file (`<dir>/<stem>-<n>`), so the
//! linker never writes to a file a running (on Windows: locked) program was started from. A
//! version's files are deleted once its process has exited; deletions that fail (Windows can
//! hold an image briefly after the process ends, or a virus scanner may have it open) are retried
//! on later sweeps. Leftovers of an earlier session are removed at the start.

use std::path::{Path, PathBuf};

/// The version files of one `velt dev --exe` session.
pub struct Versions {
    dir: PathBuf,
    stem: String,
    next: u64,
    /// Output paths (without extension) whose files are still to be deleted.
    retired: Vec<PathBuf>,
}

impl Versions {
    /// Versions of `<dir>/<stem>`; removes what an earlier session left behind.
    pub fn new(dir: PathBuf, stem: String) -> Versions {
        let mut versions = Versions {
            dir,
            stem,
            next: 0,
            retired: vec![],
        };
        versions.retired = versions.leftovers();
        versions.sweep();
        versions
    }

    /// Output path (without extension) for the next version.
    pub fn next_output(&mut self) -> PathBuf {
        self.next += 1;
        self.dir.join(format!("{}-{}", self.stem, self.next))
    }

    /// The version built to `output` (see [`Versions::next_output`]) will not run again: delete
    /// its files now or on a later sweep.
    pub fn retire(&mut self, output: PathBuf) {
        self.retired.push(output);
        self.sweep();
    }

    /// Try to delete every retired version's files again.
    pub fn sweep(&mut self) {
        self.retired.retain(|output| !remove_version(output));
    }

    /// Whether some retired files could not be deleted yet.
    pub fn pending(&self) -> bool {
        !self.retired.is_empty()
    }

    /// Versions found in the directory (from an earlier session that did not clean up).
    fn leftovers(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return vec![];
        };
        let mut outputs: Vec<PathBuf> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let version = name.strip_prefix(&self.stem)?.strip_prefix('-')?;
                let number = version.split('.').next()?;
                number.parse::<u64>().ok()?;
                Some(self.dir.join(format!("{}-{number}", self.stem)))
            })
            .collect();
        outputs.sort();
        outputs.dedup();
        outputs
    }
}

/// Delete every file of the version built to `output` (`<output>` and `<output>.*`, e.g. the
/// `.exe` and `.pdb`); whether none is left.
fn remove_version(output: &Path) -> bool {
    let (Some(dir), Some(name)) = (output.parent(), output.file_name()) else {
        return true;
    };
    let name = name.to_string_lossy();
    let prefix = format!("{name}.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    let mut all_gone = true;
    for entry in entries.flatten() {
        let file = entry.file_name();
        let file = file.to_string_lossy();
        if file == name || file.starts_with(&prefix) {
            all_gone &= std::fs::remove_file(entry.path()).is_ok();
        }
    }
    all_gone
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_versions_and_deletes_retired_and_leftover_files() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for f in [
            "main-7.exe",
            "main-7.pdb",
            "main-12",
            "main-x.exe",
            "other-1.exe",
        ] {
            std::fs::write(d.join(f), "").unwrap();
        }
        let mut versions = Versions::new(d.to_path_buf(), "main".into());
        let mut left: Vec<String> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["main-x.exe", "other-1.exe"]);

        let first = versions.next_output();
        let second = versions.next_output();
        assert_ne!(first, second);
        std::fs::write(first.with_extension("exe"), "").unwrap();
        std::fs::write(second.with_extension("exe"), "").unwrap();
        versions.retire(first.clone());
        assert!(!first.with_extension("exe").exists());
        assert!(second.with_extension("exe").exists());
        assert!(!versions.pending());
    }
}
