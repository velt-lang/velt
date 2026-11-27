//! A copy of the repository's std with one more module, for tests of how a check by units treats
//! std code std has no instance of today (`velt/probe`).

use std::path::{Path, PathBuf};

/// Copies std into `dir/std` with `probe` as the module `velt/probe` (`probe/index.velt`), and
/// gives the copy's path (for `VELT_STD`).
pub fn with_probe(dir: &Path, probe: &str) -> PathBuf {
    let std_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let copy = dir.join("std");
    copy_dir(&std_src, &copy);
    std::fs::create_dir_all(copy.join("probe")).unwrap();
    std::fs::write(copy.join("probe/index.velt"), probe).unwrap();
    copy
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
