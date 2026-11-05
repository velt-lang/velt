//! No tracked file keeps the old source extension (#2419). A branch from before the rename can
//! merge without a conflict by only adding files, and the test harnesses would not see them: a
//! golden with the old name passes by not running.

use std::path::Path;
use std::process::Command;

const PATTERN: &str = "*.vlt"; // velt-ext: keep

/// The tracked files `git ls-files` prints for the old extension, or what running it said.
fn tracked(root: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .args(["ls-files", "--", PATTERN])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().map(str::to_string).collect())
}

/// An error naming the files and the fix when any file has the old extension.
pub fn check(root: &Path) -> Result<(), String> {
    let files = tracked(root)?;
    let Some(first) = files.first() else {
        return Ok(());
    };
    Err(format!(
        "{} file(s) still have the old source extension (first: {first}). Sources are `.velt` \
         now; move this branch with `git show origin/main:scripts/velt-ext.py > \
         \"${{TMPDIR:-/tmp}}/velt-ext.py\" && python3 \"${{TMPDIR:-/tmp}}/velt-ext.py\" branch`",
        files.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn a_tracked_file_with_the_old_extension_fails_and_says_how_to_fix_it() {
        let dir = std::env::temp_dir().join(format!("xtask-old-ext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("a.velt"), "").unwrap();
        git(&dir, &["add", "."]);
        assert_eq!(check(&dir), Ok(()));

        std::fs::write(dir.join("sub/b.vlt"), "").unwrap(); // velt-ext: keep
        assert_eq!(check(&dir), Ok(()), "an untracked file is not checked");
        git(&dir, &["add", "."]);
        let err = check(&dir).unwrap_err();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(err.contains("sub/b.vlt"), "{err}"); // velt-ext: keep
        assert!(err.contains("velt-ext.py"), "{err}");
    }
}
