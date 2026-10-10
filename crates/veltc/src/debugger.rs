//! Debugger support files shipped with the toolchain: `velt_lldb.py`, the LLDB formatters for
//! Velt values. Installed as `<prefix>/share/velt/lldb/velt_lldb.py`; in a checkout it is
//! `editors/lldb/velt_lldb.py`.

use std::path::{Path, PathBuf};

/// File name of the LLDB script.
pub const LLDB_SCRIPT: &str = "velt_lldb.py";

/// The LLDB script: `$VELT_SHARE/lldb/velt_lldb.py`; else the installed layout
/// (`<prefix>/bin/velt` → `<prefix>/share/velt/lldb/`); else, walking up from the executable to
/// the first directory with a `Cargo.toml` (dev layout), its `editors/lldb/`; else the checkout
/// this binary was compiled from. `None` if none of these exists.
pub fn lldb_script() -> Option<PathBuf> {
    if let Some(share) = std::env::var_os("VELT_SHARE").filter(|s| !s.is_empty()) {
        let script = Path::new(&share).join("lldb").join(LLDB_SCRIPT);
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
        .map(|prefix| prefix.join("share/velt/lldb").join(LLDB_SCRIPT));
    let checkout = exe_dir
        .ancestors()
        .find(|d| d.join("Cargo.toml").is_file())
        .map(|repo| repo.join("editors/lldb").join(LLDB_SCRIPT));
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../editors/lldb")
        .join(LLDB_SCRIPT);
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
        let share = prefix.join("share/velt/lldb");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(&share).unwrap();
        std::fs::write(share.join(LLDB_SCRIPT), "").unwrap();
        assert_eq!(
            lldb_script_for_exe(&prefix.join("bin/velt")),
            Some(share.join(LLDB_SCRIPT))
        );

        // Dev: <repo>/target/debug/velt + <repo>/Cargo.toml + <repo>/editors/lldb.
        let repo = root.join("repo");
        let editors = repo.join("editors/lldb");
        std::fs::create_dir_all(repo.join("target/debug")).unwrap();
        std::fs::create_dir_all(&editors).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "").unwrap();
        std::fs::write(editors.join(LLDB_SCRIPT), "").unwrap();
        assert_eq!(
            lldb_script_for_exe(&repo.join("target/debug/velt")),
            Some(editors.join(LLDB_SCRIPT))
        );
    }

    #[test]
    fn this_checkout_has_the_script() {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../editors/lldb/velt_lldb.py");
        let text = std::fs::read_to_string(script).unwrap();
        assert!(text.contains("def __lldb_init_module("), "{text}");
    }
}
