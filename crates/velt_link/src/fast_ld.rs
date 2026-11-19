//! A faster linker on Linux: `cc -fuse-ld=mold` or `-fuse-ld=lld` when `mold` / `ld.lld` is on
//! `PATH`, for static links and for debug links against the shared runtime alike. The default
//! GNU ld takes seconds to link the static runtime (lld about a seventh of that), and against
//! the shared runtime it still costs twice what lld does (108M instructions against 49M for
//! hello). Not used with `$VELT_LINKER`, and a link that fails with it is retried with the
//! default linker, so an old `cc` that rejects the flag costs one failed attempt only.
//!
//! A system may have lld under a versioned name only: Debian's and Ubuntu's `lld-18` package
//! installs `ld.lld-18` and no `ld.lld` (the `lld` package adds that), and so do GitHub's Ubuntu
//! runners. `cc -fuse-ld=lld` looks for a program named `ld.lld`, so such a link gets a
//! directory of its own holding that name as a symbolic link to the versioned program, and
//! `cc -B<directory>` ([`Fast`]). The directory is made for one link, with a name no other
//! user can predict, and removed after it: a program in a shared temporary directory under a
//! fixed name could be put there by anyone.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The faster linker found on a search path.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    Mold,
    Lld,
    /// lld under a versioned name alone (`ld.lld-18`): the program.
    VersionedLld(PathBuf),
}

/// The LLVM versions a versioned lld is looked for under, newest first.
const LLD_VERSIONS: std::ops::RangeInclusive<u32> = 11..=40;

/// The fastest linker on the search path `path`, if any: mold, then lld, then the newest
/// versioned lld.
fn found_on(path: &OsStr) -> Option<Found> {
    let dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    let on_path = |name: &str| dirs.iter().any(|d| d.join(name).is_file());
    if on_path("ld.mold") || on_path("mold") {
        return Some(Found::Mold);
    }
    if on_path("ld.lld") {
        return Some(Found::Lld);
    }
    LLD_VERSIONS.rev().find_map(|v| {
        let name = format!("ld.lld-{v}");
        let program = dirs.iter().map(|d| d.join(&name)).find(|p| p.is_file())?;
        Some(Found::VersionedLld(program))
    })
}

/// What a link with the faster linker adds to the `cc` command, and what it made for it.
pub(crate) struct Fast {
    /// The arguments, before the link's own.
    pub(crate) args: Vec<OsString>,
    /// The directory made for this link ([`Found::VersionedLld`]), removed when the link is done.
    dir: Option<PathBuf>,
}

impl Drop for Fast {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// The faster link for the linker found on `PATH`, if any. Keep it until the link is done.
pub(crate) fn fast() -> Option<Fast> {
    fast_for(found_on(&std::env::var_os("PATH")?)?, &std::env::temp_dir())
}

/// The faster link with `found`; a directory it needs is made in `temp`. `None` where that
/// directory cannot be made: the link then uses the default linker.
fn fast_for(found: Found, temp: &Path) -> Option<Fast> {
    let flag = |name: &str| vec![OsString::from(format!("-fuse-ld={name}"))];
    match found {
        Found::Mold => Some(Fast {
            args: flag("mold"),
            dir: None,
        }),
        Found::Lld => Some(Fast {
            args: flag("lld"),
            dir: None,
        }),
        Found::VersionedLld(program) => {
            let dir = named_lld(&program, temp)?;
            let mut prefix = OsString::from("-B");
            prefix.push(&dir);
            Some(Fast {
                args: vec![prefix, "-fuse-ld=lld".into()],
                dir: Some(dir),
            })
        }
    }
}

/// A new directory in `temp`, this user's alone, holding `ld.lld` as a symbolic link to
/// `program`. `None` where it cannot be made (no symbolic links, a full disk).
#[cfg(unix)]
fn named_lld(program: &Path, temp: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNT: AtomicU32 = AtomicU32::new(0);
    for _ in 0..8 {
        // Not a name another user can have made first: `create` fails on one that exists, and
        // the next try has another name.
        let nanos = (SystemTime::now().duration_since(UNIX_EPOCH)).map_or(0, |d| d.subsec_nanos());
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let dir = temp.join(format!("velt-ld-{}-{n}-{nanos}", std::process::id()));
        if (std::fs::DirBuilder::new().mode(0o700).create(&dir)).is_err() {
            continue;
        }
        if std::os::unix::fs::symlink(program, dir.join("ld.lld")).is_ok() {
            return Some(dir);
        }
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    None
}

/// No faster link through a name of its own off Unix (the faster linker is Linux's).
#[cfg(not(unix))]
fn named_lld(_program: &Path, _temp: &Path) -> Option<PathBuf> {
    None
}

/// Whether a link for `os` tries the faster linker: on Linux, unless `$VELT_LINKER` names the
/// linker program (`overridden`). Whether the runtime is linked statically or shared does not
/// matter.
pub(crate) fn applies(os: crate::TargetOs, overridden: bool) -> bool {
    os == crate::TargetOs::Linux && !overridden
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TargetOs;

    /// A directory of this test's, with the empty files `names`.
    fn dir_with(root: &Path, dir: &str, names: &[&str]) -> PathBuf {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        for n in names {
            std::fs::write(d.join(n), b"").unwrap();
        }
        d
    }

    fn root(test: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("velt-fast-ld-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn path(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn prefers_mold_then_lld() {
        let root = root("order");
        let mold = dir_with(&root, "mold", &["ld.mold"]);
        let lld = dir_with(&root, "lld", &["ld.lld"]);
        let versioned = dir_with(&root, "versioned", &["ld.lld-18"]);
        assert_eq!(
            found_on(&path(&[&versioned, &lld, &mold])),
            Some(Found::Mold)
        );
        assert_eq!(found_on(&path(&[&versioned, &lld])), Some(Found::Lld));
        assert_eq!(found_on(&path(&[&root])), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// lld under a versioned name alone, as Debian's and Ubuntu's `lld-18` package and GitHub's
    /// Ubuntu runners have it, is found: the newest one, wherever on the path it is.
    #[test]
    fn finds_a_versioned_lld_the_newest_first() {
        let root = root("versioned");
        let old = dir_with(&root, "old", &["ld.lld-16", "ld.lld-17", "lld-19"]);
        let new = dir_with(&root, "new", &["ld.lld-18", "ld.lld-9", "ld.lld-x"]);
        assert_eq!(
            found_on(&path(&[&old, &new])),
            Some(Found::VersionedLld(new.join("ld.lld-18")))
        );
        assert_eq!(
            found_on(&path(&[&old])),
            Some(Found::VersionedLld(old.join("ld.lld-17")))
        );
        // A directory of that name is no program.
        let none = root.join("none");
        std::fs::create_dir_all(none.join("ld.lld-18")).unwrap();
        assert_eq!(found_on(&path(&[&none])), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mold_and_lld_by_name_need_no_directory() {
        let temp = root("flags");
        for (found, flag) in [(Found::Mold, "-fuse-ld=mold"), (Found::Lld, "-fuse-ld=lld")] {
            let fast = fast_for(found, &temp).unwrap();
            assert_eq!(fast.args, [OsString::from(flag)]);
            assert_eq!(fast.dir, None);
        }
        assert!(!temp.exists());
    }

    /// A versioned lld is linked with through a directory of the link's own that names it
    /// `ld.lld`, which is what `cc -fuse-ld=lld` looks for under `-B`; the directory is this
    /// user's alone and goes when the link is done.
    #[cfg(unix)]
    #[test]
    fn a_versioned_lld_gets_a_directory_naming_it_ld_lld() {
        use std::os::unix::fs::PermissionsExt;
        let temp = root("named");
        let bin = dir_with(&temp, "bin", &["ld.lld-18"]);
        let program = bin.join("ld.lld-18");
        let fast = fast_for(Found::VersionedLld(program.clone()), &temp).unwrap();
        let dir = fast.dir.clone().expect("a directory");
        let mut prefix = OsString::from("-B");
        prefix.push(&dir);
        assert_eq!(fast.args, [prefix, OsString::from("-fuse-ld=lld")]);
        assert_eq!(dir.parent(), Some(temp.as_path()));
        assert_eq!(std::fs::read_link(dir.join("ld.lld")).unwrap(), program);
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        // Another link at the same time has a directory of its own.
        let other = fast_for(Found::VersionedLld(program), &temp).unwrap();
        assert_ne!(other.dir, fast.dir);
        drop(fast);
        assert!(!dir.exists());
        drop(other);
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// Where the directory cannot be made, there is no faster link (the default linker links).
    #[test]
    fn no_directory_no_faster_link() {
        let temp = root("missing").join("not").join("there");
        let found = Found::VersionedLld(PathBuf::from("/usr/bin/ld.lld-18"));
        assert!(fast_for(found, &temp).is_none());
    }

    /// Debug links against the shared runtime use the faster linker too (they used GNU ld).
    #[test]
    fn applies_on_linux_unless_the_linker_is_overridden() {
        assert!(applies(TargetOs::Linux, false));
        assert!(!applies(TargetOs::Linux, true));
        assert!(!applies(TargetOs::MacOs, false));
        assert!(!applies(TargetOs::Windows, false));
    }
}
