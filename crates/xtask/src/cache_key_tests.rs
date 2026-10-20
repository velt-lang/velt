//! `.github/actions/setup/rust-cache-key.sh`: the edits that change the dependencies' compiled
//! output move the key, and the others (a release's version bump, comments, the workspace's own
//! crates) keep it. Unix only: on Windows `bash` may be WSL's, which can't run in a Windows
//! directory; the script itself has nothing specific to an OS.

use std::path::{Path, PathBuf};
use std::process::Command;

const ROOT: &str = r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "0.2.2"

[workspace.dependencies]
a = { path = "crates/a" }

[profile.dev]
opt-level = 1
"#;

const CRATE: &str = r#"[package]
name = "a"
version.workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }

[target.'cfg(unix)'.dependencies]
libc = "0.2"
"#;

const LOCK: &str = r#"version = 4

[[package]]
name = "a"
version = "0.2.2"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

/// A throwaway checkout with a workspace of one crate.
struct Repo(PathBuf);

impl Repo {
    fn new() -> Repo {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("xtask-cache-key-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Repo(dir);
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo.0)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git init");
        repo.write("rust-toolchain.toml", "[toolchain]\nchannel = \"1.90.0\"\n");
        repo.write("Cargo.toml", ROOT);
        repo.write("crates/a/Cargo.toml", CRATE);
        repo.write("Cargo.lock", LOCK);
        repo
    }

    /// Writes `path` and adds it to the index (the script lists the manifests with `git ls-files`).
    fn write(&self, path: &str, text: &str) {
        let file = self.0.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, text).unwrap();
        let ok = Command::new("git")
            .args(["add", path])
            .current_dir(&self.0)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git add {path}");
    }

    fn edit(&self, path: &str, from: &str, to: &str) {
        let text = std::fs::read_to_string(self.0.join(path)).unwrap();
        assert!(text.contains(from), "{path} has no {from:?}");
        self.write(path, &text.replacen(from, to, 1));
    }

    fn key(&self) -> String {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github/actions/setup/rust-cache-key.sh");
        let out = Command::new("bash")
            .arg(script)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let key = String::from_utf8(out.stdout).unwrap().trim().to_string();
        assert!(key.starts_with("gate-"), "{key}");
        key
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Applies `change` to a fresh checkout and says whether the key moved.
fn moves(change: impl Fn(&Repo)) -> bool {
    let repo = Repo::new();
    let before = repo.key();
    change(&repo);
    before != repo.key()
}

#[test]
fn a_release_comments_and_own_crates_keep_the_key() {
    // A release bumps the workspace version and the workspace's own entries in Cargo.lock.
    assert!(!moves(|r| {
        r.edit("Cargo.toml", "version = \"0.2.2\"", "version = \"0.3.0\"");
        r.edit("Cargo.lock", "version = \"0.2.2\"", "version = \"0.3.0\"");
    }));
    assert!(!moves(|r| r.edit(
        "Cargo.toml",
        "opt-level = 1\n",
        "# Fast enough.\nopt-level = 1  \n\n"
    )));
    // One of the workspace's own crates as a new dependency, and a new member.
    assert!(!moves(|r| {
        r.edit(
            "crates/a/Cargo.toml",
            "[dependencies]\n",
            "[dependencies]\nb = { path = \"../b\" }\n",
        );
        r.write(
            "crates/b/Cargo.toml",
            "[package]\nname = \"b\"\nversion.workspace = true\n",
        );
    }));
    // A manifest outside the workspace.
    assert!(!moves(
        |r| r.write("bench/x/Cargo.toml", "[dependencies]\ntokio = \"1\"\n")
    ));
}

#[test]
fn what_the_dependencies_are_built_with_moves_the_key() {
    assert!(moves(|r| r.edit(
        "Cargo.toml",
        "opt-level = 1",
        "opt-level = 2"
    )));
    assert!(moves(|r| r.edit(
        "Cargo.toml",
        "[profile.dev]",
        "[profile.release]\nlto = \"thin\"\n\n[profile.dev]"
    )));
    assert!(moves(|r| r.edit(
        "crates/a/Cargo.toml",
        "[\"derive\"]",
        "[]"
    )));
    assert!(moves(|r| r.edit(
        "crates/a/Cargo.toml",
        "libc = \"0.2\"",
        "libc = \"0.3\""
    )));
    assert!(moves(|r| r.edit(
        "crates/a/Cargo.toml",
        "version.workspace = true\n",
        "version.workspace = true\n\n[features]\nfast = [\"serde/std\"]\n"
    )));
    assert!(moves(|r| r.write(
        ".cargo/config.toml",
        "[build]\nrustflags = [\"-C\", \"target-cpu=native\"]\n"
    )));
    assert!(moves(|r| r.edit("Cargo.lock", "1.0.0", "1.0.1")));
    assert!(moves(|r| r.edit("rust-toolchain.toml", "1.90.0", "1.91.0")));
}
