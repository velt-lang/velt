//! Commands on several members of a workspace through the `velt` binary (#1855 step 2):
//! `--workspace`, `-p <glob>` and `--affected [<base>]`, every member at the workspace's root,
//! dependency order, one output block per member and a summary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod no_window;
mod runtime_support;
mod test_dir;

struct Repo {
    _tmp: test_dir::TestDir,
    dir: PathBuf,
}

/// `packages/core` (a library) and the programs `apps/a` and `apps/b`, which use it.
fn repo() -> Repo {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path().to_path_buf();
    let repo = Repo { _tmp: tmp, dir };
    repo.write(
        "velt.workspace.vlt",
        "export const workspace: Workspace = { members: [\"packages/*\", \"apps/*\"] };\n",
    );
    repo.write(
        "packages/core/package.vlt",
        "export const pkg: Package = { name: \"core\", version: \"0.1.0\" };\n",
    );
    repo.write(
        "packages/core/src/lib.vlt",
        "export function greet(who: string): string {\n  return `hello ${who}`;\n}\n",
    );
    for app in ["a", "b"] {
        repo.write(
            &format!("apps/{app}/package.vlt"),
            &format!("export const pkg: Package = {{ name: \"{app}\", version: \"0.1.0\", dependencies: {{ core: \"workspace\" }} }};\n"),
        );
        repo.write(
            &format!("apps/{app}/src/main.vlt"),
            &format!("import {{ greet }} from \"core\";\n\nconsole.log(greet(\"{app}\"));\n"),
        );
    }
    repo
}

impl Repo {
    fn write(&self, rel: &str, text: &str) {
        let path = self.dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn velt(&self, cwd: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
        cmd.args(args)
            .current_dir(self.dir.join(cwd))
            .env("VELT_HOME", self.dir.join("home"))
            .env_remove("VELT_REGISTRY")
            .env_remove("VELT_LAUNCHER")
            .env_remove("CI");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    /// stdout and stderr of a command that must succeed.
    fn ok(&self, cwd: &str, args: &[&str]) -> (String, String) {
        let o = self.velt(cwd, args, &[]);
        let (out, err) = (text(&o.stdout), text(&o.stderr));
        assert!(
            o.status.success(),
            "`velt {}` in {cwd}:\n{err}{out}",
            args.join(" ")
        );
        (out, err)
    }

    fn git(&self, args: &[&str]) {
        let o = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", text(&o.stderr));
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The members named on `Member`/`Failed` status lines, in order.
fn members(err: &str) -> Vec<String> {
    err.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            l.strip_prefix("Member ")
                .or_else(|| l.strip_prefix("Failed "))
                .and_then(|rest| rest.split(' ').next())
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn every_member_runs_from_the_workspace_root() {
    let r = repo();
    let (_, err) = r.ok(".", &["check"]);
    let order = members(&err);
    assert_eq!(order.len(), 3, "{err}");
    assert_eq!(
        order[0], "core",
        "a member comes after the members it uses:\n{err}"
    );
    assert!(err.contains("`velt check` on 3 members: 3 passed"), "{err}");

    // One broken member: the others still run, the summary names it, the exit code is its.
    r.write("apps/b/src/main.vlt", "const x: number = \"s\";\n");
    let o = r.velt(".", &["check", "--workspace"], &[]);
    let err = text(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("Failed b (apps/b)") && err.contains("mismatched types"),
        "{err}"
    );
    assert!(members(&err).contains(&"a".to_string()), "{err}");
    assert!(err.contains("2 passed, 1 failed (b)"), "{err}");

    // `-p` with one member runs that member alone.
    let (_, err) = r.ok(".", &["check", "-p", "a"]);
    assert_eq!(members(&err), ["a"], "{err}");
}

#[test]
fn programs_build_and_run_by_member() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let r = repo();
    let (_, err) = r.ok(".", &["build", "--workspace"]);
    assert!(err.contains("Skipping core (libraries"), "{err}");
    // A package's program is named after the package.
    for app in ["a", "b"] {
        let exe = if cfg!(windows) {
            format!("{app}.exe")
        } else {
            app.to_string()
        };
        let path = r.dir.join(format!("apps/{app}/target/velt/{exe}"));
        assert!(path.is_file(), "{err}");
    }
    assert_eq!(r.ok(".", &["run", "-p", "b"]).0, "hello b\n");
    let o = r.velt(".", &["run", "--workspace"], &[]);
    let err = text(&o.stderr);
    assert!(
        err.contains("runs one member's program") && err.contains("runnable members: a, b"),
        "{err}"
    );
}

#[test]
fn affected_members_are_the_changed_ones_and_their_dependents() {
    let r = repo();
    // As a repository keeps them: build output ignored, the lockfile committed.
    r.write(".gitignore", "target/\nhome/\n");
    r.ok(".", &["install"]);
    r.git(&["init", "-q"]);
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "start"]);
    // Nothing changed.
    let (_, err) = r.ok(".", &["check", "--affected", "HEAD"]);
    assert!(err.contains("no selected member to check"), "{err}");
    // A library: it and every member that uses it.
    r.write(
        "packages/core/src/lib.vlt",
        "export function greet(who: string): string {\n  return `hi ${who}`;\n}\n",
    );
    let (_, err) = r.ok(".", &["check", "--affected", "HEAD"]);
    assert_eq!(members(&err).len(), 3, "{err}");
    r.git(&["commit", "-q", "-am", "core"]);
    // A program: that member alone, untracked files included.
    r.write("apps/a/src/extra.vlt", "export const x = 1;\n");
    let (_, err) = r.ok(".", &["check", "--affected=HEAD"]);
    assert_eq!(members(&err), ["a"], "{err}");
    // In CI the base must be named.
    let o = r.velt(".", &["check", "--affected"], &[("CI", "true")]);
    assert!(
        text(&o.stderr).contains("in CI, name the base"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn a_selection_outside_a_workspace_or_with_a_file_is_an_error() {
    let r = repo();
    std::fs::remove_file(r.dir.join("velt.workspace.vlt")).unwrap();
    let o = r.velt("apps/a", &["check", "--workspace"], &[]);
    assert!(
        text(&o.stderr).contains("select members of a workspace"),
        "{}",
        text(&o.stderr)
    );
    let r = repo();
    let o = r.velt(".", &["check", "-p", "a", "apps/a/src/main.vlt"], &[]);
    assert!(
        text(&o.stderr).contains("leave out `apps/a/src/main.vlt`"),
        "{}",
        text(&o.stderr)
    );
}
