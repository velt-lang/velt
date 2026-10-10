//! Debugger support files shipped with the toolchain: `velt_lldb.py`, the LLDB formatters for
//! Velt values. Installed as `<prefix>/share/velt/lldb/velt_lldb.py`; in a checkout it is
//! `editors/lldb/velt_lldb.py`.

use std::path::{Path, PathBuf};

/// File name of the LLDB script.
pub const LLDB_SCRIPT: &str = "velt_lldb.py";

/// `<prefix>/share/velt/lldb/velt_lldb.py` of the toolchain installed at `prefix`.
pub fn installed_lldb_script(prefix: &Path) -> PathBuf {
    join(prefix, &["share", "velt", "lldb", LLDB_SCRIPT])
}

/// `<repo>/editors/lldb/velt_lldb.py` of a checkout.
fn checkout_lldb_script(repo: &Path) -> PathBuf {
    join(repo, &["editors", "lldb", LLDB_SCRIPT])
}

/// `base` joined with each of `parts` (one component at a time, so Windows paths get one kind of
/// separator), lexically normalized.
fn join(base: &Path, parts: &[&str]) -> PathBuf {
    let mut path = base.to_path_buf();
    path.extend(parts);
    vpm::relpath::normalize(&path)
}

/// The LLDB script: `$VELT_SHARE/lldb/velt_lldb.py`; else the installed layout
/// (`<prefix>/bin/velt` → `<prefix>/share/velt/lldb/`); else, walking up from the executable to
/// the first directory with a `Cargo.toml` (dev layout), its `editors/lldb/`; else the checkout
/// this binary was compiled from. `None` if none of these exists.
pub fn lldb_script() -> Option<PathBuf> {
    if let Some(share) = std::env::var_os("VELT_SHARE").filter(|s| !s.is_empty()) {
        let script = join(Path::new(&share), &["lldb", LLDB_SCRIPT]);
        return script.is_file().then_some(script);
    }
    lldb_script_for_exe(&std::env::current_exe().ok()?)
}

/// Testable core of [`lldb_script`] (without `$VELT_SHARE`) for the executable at `exe`.
fn lldb_script_for_exe(exe: &Path) -> Option<PathBuf> {
    let exe_dir = exe.parent()?;
    let installed = exe_dir
        .file_name()
        .filter(|n| *n == "bin")
        .and_then(|_| exe_dir.parent())
        .map(installed_lldb_script);
    let checkout = exe_dir
        .ancestors()
        .find(|d| d.join("Cargo.toml").is_file())
        .map(checkout_lldb_script);
    let source = checkout_lldb_script(&join(Path::new(env!("CARGO_MANIFEST_DIR")), &["..", ".."]));
    [installed, checkout, Some(source)]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_installed_and_dev_layouts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Installed: <prefix>/bin/velt + <prefix>/share/velt/lldb/velt_lldb.py.
        let prefix = root.join("prefix");
        let script = installed_lldb_script(&prefix);
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "").unwrap();
        assert_eq!(
            lldb_script_for_exe(&prefix.join("bin").join("velt")),
            Some(script)
        );

        // Dev: <repo>/target/debug/velt + <repo>/Cargo.toml + <repo>/editors/lldb.
        let repo = root.join("repo");
        let script = checkout_lldb_script(&repo);
        std::fs::create_dir_all(repo.join("target").join("debug")).unwrap();
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "").unwrap();
        std::fs::write(&script, "").unwrap();
        let found = lldb_script_for_exe(&repo.join("target").join("debug").join("velt"));
        assert_eq!(found, Some(script));
    }

    /// One kind of separator, no `..` (Windows showed `prefix\share/velt/lldb\velt_lldb.py`).
    #[test]
    fn paths_are_joined_per_component() {
        let prefix = Path::new("p").join("x").join("..");
        let script = installed_lldb_script(&prefix);
        assert_eq!(
            script,
            Path::new("p")
                .join("share")
                .join("velt")
                .join("lldb")
                .join(LLDB_SCRIPT)
        );
        assert!(!script
            .to_string_lossy()
            .contains(if cfg!(windows) { '/' } else { '\\' }));
    }
}
