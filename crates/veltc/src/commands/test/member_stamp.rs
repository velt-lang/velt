//! A member's test stamp (#1855 step 3): `velt test --workspace` with nothing changed starts no
//! `velt` for a member whose last `velt test` passed with every test file's result kept
//! (`cache`: none reaches outside the program).
//!
//! After such a run, `velt test` writes `target/velt/test/member-stamp` in the build stamp's
//! format: a key over the command line, the `velt` that ran it and the `VELT_*` environment;
//! as `objects`, how many tests passed; and as entries, every entry of its harnesses' build stamps
//! (each source file, directory and probe, and the programs themselves) and every directory test
//! discovery listed. The workspace command (`commands::members::run`) checks it with the build
//! stamp's own check before it would start the member: unchanged, the member passed again.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use velt_build_cache::{Key, KeyHasher};

use crate::driver::stamp;

/// The member stamp of the package at `root`.
fn path(root: &Path) -> PathBuf {
    root.join("target")
        .join("velt")
        .join("test")
        .join("member-stamp")
}

/// The key of `velt <args>` (the command and its options) run by this `velt` now.
pub(crate) fn key(args: &[OsString]) -> Key {
    let mut h = KeyHasher::new();
    h.part(b"velt-member-stamp 1");
    let exe = std::env::current_exe().ok();
    let stat = exe.as_deref().and_then(vpm::probe::stat);
    h.part(format!("{exe:?} {stat:?}").as_bytes());
    for arg in args {
        h.part(arg.to_string_lossy().as_bytes());
    }
    h.part(format!("{:?}", stamp::velt_env()).as_bytes());
    h.finish()
}

/// Record that `velt <args>` in the package at `root` passed `passed` tests with every result
/// kept, from the programs `harnesses` and the directories discovery listed, `listed`. Best
/// effort: without it the member just runs.
pub(super) fn write(
    root: &Path,
    args: &[OsString],
    passed: usize,
    harnesses: &[PathBuf],
    listed: &[(PathBuf, String)],
) {
    // A directory that changed during the run (a test file added, or the first run making
    // `target/`) makes the stamp untrustworthy: none, and the next run writes one.
    let unchanged = listed
        .iter()
        .all(|(dir, hash)| vpm::probe::listing_hash(dir).as_deref() == Some(hash.as_str()));
    if !unchanged {
        remove(root);
        return;
    }
    let mut entries: Vec<String> = vec![];
    for exe in harnesses {
        let Ok(text) = std::fs::read_to_string(stamp::path(exe)) else {
            return;
        };
        entries.extend(stamp::entry_lines(&text).into_iter().map(str::to_string));
    }
    entries.extend(listed.iter().map(|(dir, hash)| stamp::dir_entry(dir, hash)));
    entries.sort();
    entries.dedup();
    let text = stamp::compose(&key(args), passed, &entries);
    let _ = stamp::write(&path(root), &text);
}

/// Forget the member stamp of the package at `root`: a run that didn't qualify leaves none.
pub(super) fn remove(root: &Path) {
    let _ = std::fs::remove_file(path(root));
}

/// How many tests passed, when the member stamp of the package at `root` is of `velt <args>`
/// and nothing it records changed.
pub(crate) fn up_to_date(root: &Path, args: &[OsString]) -> Option<usize> {
    if !stamp::enabled() {
        return None;
    }
    stamp::still_valid(&path(root), &key(args))
}
