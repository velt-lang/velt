//! `--affected [<base>]`: the members with a file changed since `base`, committed or not (and
//! untracked files), plus every member that depends on them. A change to the workspace file or
//! its lockfile affects every member. Without a base, the merge base with the upstream branch;
//! in CI (`CI` set) the base must be named, since a CI checkout often has no upstream branch, or a
//! shallow one.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use vpm::workspace::{Member, Workspace, WORKSPACE_FILE};

use super::{dependents, shown};

/// The names of the members a change since `base` affects.
pub(super) fn members<'w>(
    ws: &'w Workspace,
    base: Option<&str>,
) -> Result<BTreeSet<&'w str>, String> {
    let base = match base {
        Some(b) => b.to_string(),
        None => default_base(&ws.root)?,
    };
    let mut files = git(
        &ws.root,
        &["diff", "-z", "--name-only", "--relative", &base, "--"],
    )?;
    files.extend(git(
        &ws.root,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )?);
    let all: BTreeSet<&str> = ws.members.iter().map(Member::name).collect();
    let mut touched = BTreeSet::new();
    for file in &files {
        if file == WORKSPACE_FILE || file == vpm::lockfile::LOCK_FILE {
            return Ok(all);
        }
        if let Some(m) = owner(ws, file) {
            touched.insert(m.name());
        }
    }
    let dependents = dependents(ws);
    let mut out = touched.clone();
    for name in touched {
        out.extend(dependents.get(name).into_iter().flatten().copied());
    }
    Ok(out)
}

/// The innermost member whose directory holds `file` (relative to the workspace root), unless it
/// is in that member's `target/`: build output is velt's, not the member's sources.
fn owner<'w>(ws: &'w Workspace, file: &str) -> Option<&'w Member> {
    let (rel, m) = ws
        .members
        .iter()
        .filter_map(|m| {
            let rel = shown(ws, &m.dir);
            let prefix = if rel == "." {
                String::new()
            } else {
                format!("{rel}/")
            };
            file.starts_with(&prefix).then_some((prefix, m))
        })
        .max_by_key(|(prefix, _)| prefix.len())?;
    (!file[rel.len()..].starts_with("target/")).then_some(m)
}

/// The merge base of `HEAD` and its upstream branch.
fn default_base(root: &Path) -> Result<String, String> {
    if std::env::var_os("CI").is_some_and(|v| !v.is_empty()) {
        return Err(
            "in CI, name the base `--affected` compares with: `--affected origin/main` (a CI checkout often has no upstream branch)".to_string(),
        );
    }
    let base = git(root, &["merge-base", "HEAD", "@{upstream}"]).map_err(|_| {
        "`--affected` compares with the merge base of the upstream branch, and this branch has none: name the base, `--affected <branch or commit>`".to_string()
    })?;
    base.into_iter()
        .next()
        .map(|b| b.trim().to_string())
        .ok_or_else(|| "git found no merge base with the upstream branch".to_string())
}

/// git's output in `dir`, split at NULs (`-z`) or line ends.
fn git(dir: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("`--affected` needs git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err
            .lines()
            .find(|l| l.starts_with("fatal:"))
            .or(err.lines().last())
            .unwrap_or("git failed");
        return Err(format!("`--affected`: {}", line.trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let sep = if args.contains(&"-z") { '\0' } else { '\n' };
    Ok(text
        .split(sep)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}
