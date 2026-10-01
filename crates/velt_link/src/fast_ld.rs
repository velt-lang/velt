//! A faster linker for static links on Linux: `cc -fuse-ld=mold` or `-fuse-ld=lld` when `mold` /
//! `ld.lld` is on `PATH` (the default GNU ld takes seconds to link the static runtime; lld about
//! a seventh of that). Not used with `$VELT_LINKER`, and a link that fails with it is retried
//! with the default linker, so an old `cc` that rejects the flag costs one failed attempt only.

/// The `-fuse-ld=` flag for the fastest linker found on `PATH`, if any.
pub(crate) fn fuse_ld_flag() -> Option<&'static str> {
    let path = std::env::var_os("PATH")?;
    let on_path = |name: &str| std::env::split_paths(&path).any(|d| d.join(name).is_file());
    if on_path("ld.mold") || on_path("mold") {
        Some("-fuse-ld=mold")
    } else if on_path("ld.lld") {
        Some("-fuse-ld=lld")
    } else {
        None
    }
}
