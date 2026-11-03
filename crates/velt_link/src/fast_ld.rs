//! A faster linker on Linux: `cc -fuse-ld=mold` or `-fuse-ld=lld` when `mold` / `ld.lld` is on
//! `PATH`, for static links and for debug links against the shared runtime alike. The default
//! GNU ld takes seconds to link the static runtime (lld about a seventh of that), and against
//! the shared runtime it still costs twice what lld does (108M instructions against 49M for
//! hello). Not used with `$VELT_LINKER`, and a link that fails with it is retried with the
//! default linker, so an old `cc` that rejects the flag costs one failed attempt only.

use std::ffi::OsStr;

/// The `-fuse-ld=` flag for the fastest linker found on `PATH`, if any.
pub(crate) fn fuse_ld_flag() -> Option<&'static str> {
    fuse_ld_flag_on(&std::env::var_os("PATH")?)
}

/// [`fuse_ld_flag`] for the search path `path`.
fn fuse_ld_flag_on(path: &OsStr) -> Option<&'static str> {
    let on_path = |name: &str| std::env::split_paths(path).any(|d| d.join(name).is_file());
    if on_path("ld.mold") || on_path("mold") {
        Some("-fuse-ld=mold")
    } else if on_path("ld.lld") {
        Some("-fuse-ld=lld")
    } else {
        None
    }
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

    #[test]
    fn prefers_mold_then_lld() {
        let dir = std::env::temp_dir().join(format!("velt-fast-ld-{}", std::process::id()));
        let (mold, lld) = (dir.join("mold"), dir.join("lld"));
        std::fs::create_dir_all(&mold).unwrap();
        std::fs::create_dir_all(&lld).unwrap();
        std::fs::write(mold.join("ld.mold"), b"").unwrap();
        std::fs::write(lld.join("ld.lld"), b"").unwrap();
        let path = |dirs: &[&std::path::Path]| std::env::join_paths(dirs).unwrap();
        assert_eq!(
            fuse_ld_flag_on(&path(&[&lld, &mold])),
            Some("-fuse-ld=mold")
        );
        assert_eq!(fuse_ld_flag_on(&path(&[&lld])), Some("-fuse-ld=lld"));
        assert_eq!(fuse_ld_flag_on(&path(&[&dir])), None);
        let _ = std::fs::remove_dir_all(&dir);
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
