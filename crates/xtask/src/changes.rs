//! The files a change touches: everything that differs from where the branch left its base
//! (`git merge-base <base> HEAD`), plus uncommitted and untracked files.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use crate::comments;

pub struct Changes {
    /// Relative to the repository root with `/` separators, sorted and deduplicated. Renames
    /// count as a deletion and an addition, so both paths are seen.
    pub paths: Vec<String>,
    /// The Rust files under `crates/` among `paths` whose code is unchanged: only comments
    /// differ (crates/xtask/src/comments.rs).
    pub comment_only: BTreeSet<String>,
}

/// The default base: `origin/main`, else a local `main`.
pub fn default_base(root: &Path) -> Option<String> {
    ["origin/main", "main"]
        .into_iter()
        .find(|rev| git(root, &["rev-parse", "--verify", "--quiet", rev]).is_ok())
        .map(String::from)
}

/// The changes since the merge base with `base`, in the working tree.
pub fn changes(root: &Path, base: &str) -> Result<Changes, String> {
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
    let comment_only = paths
        .iter()
        .filter(|p| p.starts_with("crates/") && p.ends_with(".rs"))
        .filter(|p| {
            let old = git(root, &["show", &format!("{merge_base}:{p}")]);
            let new = std::fs::read_to_string(root.join(p));
            matches!((old, new), (Ok(old), Ok(new)) if comments::same_code(&old, &new))
        })
        .cloned()
        .collect();
    Ok(Changes {
        paths,
        comment_only,
    })
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
