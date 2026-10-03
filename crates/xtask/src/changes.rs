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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A throwaway repository: `main` with a commit, then a branch `work` checked out.
    struct Repo(PathBuf);

    impl Repo {
        fn new(files: &[(&str, &str)]) -> Repo {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir =
                std::env::temp_dir().join(format!("xtask-changes-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let repo = Repo(dir);
            repo.git(&["init", "-q", "-b", "main"]);
            for (path, text) in files {
                repo.write(path, text.as_bytes());
            }
            repo.git(&["add", "-A"]);
            repo.git(&["commit", "-q", "-m", "base"]);
            repo.git(&["checkout", "-q", "-b", "work"]);
            repo
        }

        fn write(&self, path: &str, bytes: &[u8]) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        fn git(&self, args: &[&str]) {
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
                .current_dir(&self.0)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn only_edited_files_whose_code_is_unchanged_are_comment_only() {
        let code = "fn f() {} // one\n";
        let repo = Repo::new(&[
            ("crates/a/src/committed.rs", code),
            ("crates/a/src/uncommitted.rs", code),
            ("crates/a/src/code.rs", code),
            ("crates/a/src/deleted.rs", code),
            ("crates/a/src/renamed.rs", code),
            ("crates/a/src/bytes.rs", code),
            ("crates/a/notes.txt", "one\n"),
        ]);
        // Comment edits: one committed on the branch, one left in the working tree.
        repo.write(
            "crates/a/src/committed.rs",
            b"/// Docs.\nfn f() {} // two\n",
        );
        repo.git(&["commit", "-q", "-am", "comment"]);
        repo.write("crates/a/src/uncommitted.rs", b"fn f() {}\n");
        // Never comment-only: a code edit, a deletion, both sides of a rename, a file that is
        // no longer UTF-8, a new file (even one of comments), and a file that isn't Rust.
        repo.write("crates/a/src/code.rs", b"fn g() {} // one\n");
        repo.git(&["rm", "-q", "crates/a/src/deleted.rs"]);
        repo.git(&["mv", "crates/a/src/renamed.rs", "crates/a/src/moved.rs"]);
        repo.write("crates/a/src/bytes.rs", b"fn f() {} // \xff\n");
        repo.write("crates/a/src/new.rs", b"// only a comment\n");
        repo.write("crates/a/notes.txt", b"two\n");

        let c = changes(&repo.0, "main").unwrap();
        assert_eq!(
            c.paths,
            [
                "crates/a/notes.txt",
                "crates/a/src/bytes.rs",
                "crates/a/src/code.rs",
                "crates/a/src/committed.rs",
                "crates/a/src/deleted.rs",
                "crates/a/src/moved.rs",
                "crates/a/src/new.rs",
                "crates/a/src/renamed.rs",
                "crates/a/src/uncommitted.rs",
            ]
        );
        let comment_only: Vec<&str> = c.comment_only.iter().map(String::as_str).collect();
        assert_eq!(
            comment_only,
            ["crates/a/src/committed.rs", "crates/a/src/uncommitted.rs"]
        );
    }
}
