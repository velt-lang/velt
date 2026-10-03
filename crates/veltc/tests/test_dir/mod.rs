//! A temporary directory for a test that builds or runs programs, removed when dropped.
//!
//! `tempfile` ignores a removal that fails. On Windows that left directories behind whenever a
//! program still held a file in them; a `velt dev` session's are about 115 MB each (the shared
//! runtime with its debug info). Here a removal that still fails once the files are released
//! fails the test (or is reported, when the test is already failing).

use std::path::Path;
use std::time::Duration;

/// A temporary directory named `velt-test-*`, removed when dropped. Drop whatever runs programs
/// in it (a `velt dev` session) first.
pub struct TestDir(Option<tempfile::TempDir>);

impl TestDir {
    /// A new directory in the system's temporary directory.
    pub fn new() -> TestDir {
        let dir = tempfile::Builder::new().prefix("velt-test-").tempdir();
        TestDir(Some(dir.expect("temp dir")))
    }

    /// Where it is.
    pub fn path(&self) -> &Path {
        self.0.as_ref().expect("ICE: removed").path()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let Some(dir) = self.0.take() else { return };
        let path = dir.keep();
        if let Err(e) = remove(&path) {
            let msg = format!("cannot remove the test directory {}: {e}", path.display());
            if std::thread::panicking() {
                eprintln!("{msg}");
            } else {
                panic!("{msg}");
            }
        }
    }
}

/// How long Windows may take to release the files of programs that have exited (a hang guard).
const RELEASE_LIMIT: Duration = Duration::from_secs(60);

/// Remove `dir` and everything in it. On Windows the executable and DLLs of a program that has
/// exited, and been waited for, can stay locked for a few more milliseconds (the system tears
/// down the image mapping, an antivirus scans the closed files): a removal that fails because a
/// file is in use is repeated until it succeeds or [`RELEASE_LIMIT`] has passed.
fn remove(dir: &Path) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + RELEASE_LIMIT;
    let mut retries = 0;
    loop {
        match std::fs::remove_dir_all(dir) {
            Err(e) if released_soon(&e) && std::time::Instant::now() < deadline => {
                retries += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            result => {
                if retries > 0 {
                    eprintln!(
                        "note: {} could be removed after {retries} retries",
                        dir.display()
                    );
                }
                return result;
            }
        }
    }
}

/// Whether `e` says a file is still in use: access denied (5), a sharing violation (32) or a
/// directory that isn't empty yet because a deletion is pending (145). Windows only.
fn released_soon(e: &std::io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 145))
}
