//! The files a change touches: everything that differs from where the branch left its base
//! (`git merge-base <base> HEAD`), plus uncommitted and untracked files.

use std::path::Path;
use std::process::Command;

/// The default base: `origin/main`, else a local `main`.
pub fn default_base(root: &Path) -> Option<String> {
    ["origin/main", "main"]
        .into_iter()
        .find(|rev| git(root, &["rev-parse", "--verify", "--quiet", rev]).is_ok())
        .map(String::from)
}

/// Changed paths, relative to the repository root with `/` separators, sorted and deduplicated.
/// Renames count as a deletion and an addition, so both paths are seen.
pub fn changed_paths(root: &Path, base: &str) -> Result<Vec<String>, String> {
    let merge_base = git(root, &["merge-base", base, "HEAD"])
        .map_err(|e| format!("no merge base with `{base}` ({e}); fetch it, or pass --full"))?;
    let merge_base = merge_base.trim();
    let mut paths = vec![];
    for args in [
        &["diff", "--name-only", "--no-renames", merge_base, "HEAD"][..],
        &["diff", "--name-only", "--no-renames", "HEAD"],
        &["ls-files", "--others", "--exclude-standard"],
    ] {
        paths.extend(
            git(root, args)?
                .lines()
                .map(|l| l.trim().replace('\\', "/")),
        );
    }
    paths.retain(|p| !p.is_empty());
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {}: {}", args.join(" "), err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
