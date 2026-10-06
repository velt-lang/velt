//! Where a test builds its programs: `target/<name>` in the workspace, or `<name>` inside
//! `VELT_GOLDEN_WORK` when that is set, so that one variable moves every test's build output
//! (e.g. to a disk with more space), not only the main golden run's.

use std::path::{Path, PathBuf};

/// The build directory named `name` for a test whose workspace root is `root`.
pub fn work_dir(root: &Path, name: &str) -> PathBuf {
    match std::env::var_os("VELT_GOLDEN_WORK") {
        Some(dir) => PathBuf::from(dir).join(name),
        None => root.join("target").join(name),
    }
}
