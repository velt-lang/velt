//! `cargo xtask install-hooks`: a `pre-push` git hook that runs `cargo xtask precheck`.
//!
//! The hook goes where git looks for hooks (`git rev-parse --git-path hooks`: the shared
//! `.git/hooks` of every worktree, or `core.hooksPath`), so one install covers every worktree.
//! It runs the precheck of the worktree being pushed from, and does nothing in a checkout whose
//! xtask has no precheck yet. `git push --no-verify` skips it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The line that marks a hook as ours: an install replaces a hook with it, never one without.
const MARKER: &str = "# velt: installed by `cargo xtask install-hooks`";

/// The hook script.
pub fn script() -> String {
    format!(
        "#!/bin/sh\n\
         {MARKER} (crates/xtask/src/hooks.rs).\n\
         # Runs the fast checks on the changed files before a push; `git push --no-verify` skips it.\n\
         # Git runs hooks in the top directory of the worktree being pushed from.\n\
         if [ ! -f crates/xtask/src/precheck.rs ]; then\n\
         \x20 exit 0  # a branch from before `cargo xtask precheck`\n\
         fi\n\
         exec cargo xtask precheck\n"
    )
}

/// The directory git reads hooks from, for the repository at `root`.
fn hooks_dir(root: &Path) -> Result<PathBuf, String> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse --git-path hooks: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    // Relative to the directory git ran in.
    Ok(root.join(dir))
}

/// What [`install`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    New(PathBuf),
    Updated(PathBuf),
    Unchanged(PathBuf),
    /// Replaced a hook that was not ours (`--force`), kept at the second path.
    Replaced(PathBuf, PathBuf),
}

/// Writes the `pre-push` hook. A `pre-push` hook that isn't ours is left alone unless `force`,
/// which keeps it as `pre-push.orig` (or `pre-push.orig.<n>` when that is taken: an earlier
/// saved hook is never overwritten).
pub fn install(root: &Path, force: bool) -> Result<Installed, String> {
    let dir = hooks_dir(root)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let hook = dir.join("pre-push");
    let want = script();
    let result = match std::fs::read_to_string(&hook) {
        Ok(text) if text == want => return Ok(Installed::Unchanged(hook)),
        Ok(text) if text.contains(MARKER) => Installed::Updated(hook.clone()),
        Ok(_) if !force => {
            return Err(format!(
                "{} already exists and was not installed by `cargo xtask install-hooks`; it was \
                 left alone.\nAdd `cargo xtask precheck` to it yourself, or rerun with --force to \
                 replace it (the old hook is kept as pre-push.orig, or pre-push.orig.<n>).",
                hook.display()
            ))
        }
        Ok(_) => {
            let orig = free_backup_name(&dir);
            std::fs::rename(&hook, &orig).map_err(|e| format!("{}: {e}", hook.display()))?;
            Installed::Replaced(hook.clone(), orig)
        }
        Err(_) if hook.exists() => {
            return Err(format!(
                "{} exists but cannot be read; move it away and rerun",
                hook.display()
            ))
        }
        Err(_) => Installed::New(hook.clone()),
    };
    std::fs::write(&hook, want).map_err(|e| format!("{}: {e}", hook.display()))?;
    make_executable(&hook)?;
    Ok(result)
}

/// `pre-push.orig`, else the first free `pre-push.orig.<n>`.
fn free_backup_name(dir: &Path) -> PathBuf {
    let first = dir.join("pre-push.orig");
    if !first.exists() {
        return first;
    }
    (1..)
        .map(|n| dir.join(format!("pre-push.orig.{n}")))
        .find(|p| !p.exists())
        .expect("ICE: an unbounded range always yields a free name")
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Git for Windows runs hooks with its own `sh`, whatever the file's permissions.
#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<(), String> {
    Ok(())
}

/// The message for the user.
pub fn describe(installed: &Installed) -> String {
    let tail = "It runs `cargo xtask precheck` before every push from any worktree; \
                `git push --no-verify` skips it.";
    match installed {
        Installed::New(p) => format!("installed {}\n{tail}", p.display()),
        Installed::Updated(p) => format!("updated {}\n{tail}", p.display()),
        Installed::Unchanged(p) => format!("{} is already installed\n{tail}", p.display()),
        Installed::Replaced(p, orig) => format!(
            "installed {} (the previous hook is now {})\n{tail}",
            p.display(),
            orig.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    fn temp_repo(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let base =
            std::env::temp_dir().join(format!("xtask-hooks-{tag}-{}-{nanos}", std::process::id()));
        let repo = base.join("main");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        base
    }

    #[test]
    fn installs_once_into_the_hooks_every_worktree_shares() {
        let base = temp_repo("shared");
        let main = base.join("main");
        let tree = base.join("tree");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                tree.to_str().unwrap(),
                "-b",
                "work",
            ],
        );
        // Installed from a worktree, it lands in the main repository's hooks.
        let first = install(&tree, false).unwrap();
        let hook = main.join(".git").join("hooks").join("pre-push");
        assert!(
            matches!(&first, Installed::New(p) if p.canonicalize().unwrap() == hook.canonicalize().unwrap())
        );
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), script());
        // Again, from the main checkout: nothing to do.
        assert!(matches!(
            install(&main, false).unwrap(),
            Installed::Unchanged(_)
        ));
        // An older version of ours is replaced without asking.
        std::fs::write(&hook, format!("#!/bin/sh\n{MARKER}\nexit 0\n")).unwrap();
        assert!(matches!(
            install(&main, false).unwrap(),
            Installed::Updated(_)
        ));
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), script());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn someone_elses_hook_is_kept_unless_forced() {
        let base = temp_repo("foreign");
        let main = base.join("main");
        let hooks = main.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let theirs = "#!/bin/sh\necho mine\n";
        std::fs::write(hooks.join("pre-push"), theirs).unwrap();
        let err = install(&main, false).unwrap_err();
        assert!(
            err.contains("left alone") && err.contains("--force"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-push")).unwrap(),
            theirs
        );
        assert!(matches!(
            install(&main, true).unwrap(),
            Installed::Replaced(..)
        ));
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-push")).unwrap(),
            script()
        );
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-push.orig")).unwrap(),
            theirs
        );
        // Forcing again over another foreign hook keeps both saved hooks.
        let second = "#!/bin/sh\necho second\n";
        std::fs::write(hooks.join("pre-push"), second).unwrap();
        assert!(matches!(
            install(&main, true).unwrap(),
            Installed::Replaced(_, orig) if orig.ends_with("pre-push.orig.1")
        ));
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-push.orig")).unwrap(),
            theirs
        );
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-push.orig.1")).unwrap(),
            second
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn core_hooks_path_is_respected() {
        let base = temp_repo("hookspath");
        let main = base.join("main");
        git(&main, &["config", "core.hooksPath", "githooks"]);
        install(&main, false).unwrap();
        assert!(main.join("githooks").join("pre-push").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }
}
