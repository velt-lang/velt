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
    for args in [&["run", "--workspace"][..], &["run"], &["dev"]] {
        let o = r.velt(".", args, &[]);
        let err = text(&o.stderr);
        assert!(
            err.contains("runs one member's program") && err.contains("runnable members: a, b"),
            "{args:?}: {err}"
        );
    }
    // A relative `-o` is a path from where velt started, not from the member.
    r.ok(".", &["build", "-p", "a", "-o", "dist/a"]);
    let exe = if cfg!(windows) {
        "dist/a.exe"
    } else {
        "dist/a"
    };
    assert!(r.dir.join(exe).is_file());
    // Options that can't serve several members.
    for args in [
        &["build", "--workspace", "-o", "x"][..],
        &["test", "--workspace", "--watch"],
        &["check", "--workspace", "--json"],
    ] {
        let o = r.velt(".", args, &[]);
        let err = text(&o.stderr);
        assert!(err.contains("with several members"), "{args:?}: {err}");
    }
    // `velt run <file> <program args>`: the program's `-p` and `--workspace` are its own.
    r.write(
        "loose/s.vlt",
        "console.log(process.argv.slice(2).join(\",\"));\n",
    );
    std::fs::remove_file(r.dir.join("velt.workspace.vlt")).unwrap();
    let (out, _) = r.ok("loose", &["run", "s.vlt", "-p", "8080", "--workspace"]);
    assert_eq!(out, "-p,8080,--workspace\n");
}

#[test]
fn affected_members_are_the_changed_ones_and_their_dependents() {
    let r = repo();
    // The lockfile committed; build output in `target/` not ignored, which must not count.
    r.write(".gitignore", "home/\n");
    r.ok(".", &["install"]);
    r.git(&["init", "-q"]);
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "start"]);
    // Nothing changed but build output.
    r.write("apps/a/target/velt/a.o", "object");
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

#[test]
fn formatting_members_needs_no_registry() {
    let r = repo();
    r.write(
        "apps/b/package.vlt",
        "export const pkg: Package = { name: \"b\", version: \"0.1.0\", dependencies: { core: \"workspace\", nosuchpkg: \"1\" } };\n",
    );
    let o = r.velt(".", &["fmt"], &[("VELT_REGISTRY", "http://127.0.0.1:9")]);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(err.contains("`velt fmt` on 3 members: 3 passed"), "{err}");
}

#[test]
fn a_scoped_member_is_selected_by_its_name_without_the_scope() {
    let r = repo();
    r.write(
        "apps/a/package.vlt",
        "export const pkg: Package = { name: \"@org/server\", version: \"0.1.0\", dependencies: { core: \"workspace\" } };\n",
    );
    let (_, err) = r.ok(".", &["check", "-p", "serv*"]);
    assert_eq!(members(&err), ["@org/server"], "{err}");
}

/// The members a run started (`Member`/`Failed` lines) and those it found unchanged, by name.
fn ran_and_unchanged(err: &str) -> (Vec<String>, Vec<String>) {
    let name = |l: &str, prefix: &str| {
        l.trim_start()
            .strip_prefix(prefix)
            .and_then(|rest| rest.split([' ', ':']).next())
            .map(str::to_string)
    };
    let ran = err
        .lines()
        .filter_map(|l| name(l, "Member ").or_else(|| name(l, "Failed ")))
        .collect();
    let unchanged = err.lines().filter_map(|l| name(l, "Unchanged ")).collect();
    (ran, unchanged)
}

#[test]
fn members_whose_tests_cannot_have_changed_are_not_started() {
    let r = repo();
    r.write(
        "packages/core/tests/add.test.vlt",
        "import { greet } from \"../src/lib\";\nimport { expect } from \"velt/test\";\n\nexport function test_greet() {\n  expect(greet(\"x\")).toBe(\"hello x\");\n}\n",
    );
    r.write(
        "apps/a/tests/a.test.vlt",
        "import { greet } from \"core\";\nimport { expect } from \"velt/test\";\n\nexport function test_a() {\n  expect(greet(\"a\")).toBe(\"hello a\");\n}\n",
    );
    r.write(
        "apps/b/tests/b.test.vlt",
        "import { expect } from \"velt/test\";\n\nexport function test_clock() {\n  expect(Date.now() > 0).toBe(true);\n}\n",
    );
    r.write("packages/core/tests/fixtures/README.md", "data\n");
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    // The first run makes each member's `target/`, which changes the directory discovery listed,
    // so it leaves no member stamp; the second does.
    for _ in 0..2 {
        let (ran, unchanged) = ran_and_unchanged(&r.ok(".", &["test"]).1);
        assert_eq!(
            (sorted(ran), unchanged.len()),
            (vec!["a".into(), "b".into(), "core".into()], 0)
        );
    }
    // Nothing changed: `core` and `a` pass again without a `velt`; `b` reads the clock.
    let (ran, unchanged) = ran_and_unchanged(&r.ok(".", &["test"]).1);
    assert_eq!(ran, ["b"]);
    assert_eq!(sorted(unchanged), ["a", "core"]);
    // A change in `core` runs it and `a`, which imports it.
    r.write(
        "packages/core/src/lib.vlt",
        "export function greet(who: string): string {\n  return \"hello \" + who;\n}\n",
    );
    let (ran, _) = ran_and_unchanged(&r.ok(".", &["test"]).1);
    assert_eq!(sorted(ran), ["a", "b", "core"]);
    // A new test file is a change too.
    r.write(
        "apps/a/tests/more.test.vlt",
        "import { expect } from \"velt/test\";\n\nexport function test_more() {\n  expect(1).toBe(1);\n}\n",
    );
    let (ran, unchanged) = ran_and_unchanged(&r.ok(".", &["test"]).1);
    assert_eq!(sorted(ran), ["a", "b"]);
    assert_eq!(unchanged, ["core"]);
    // So is one in a directory no test file was in, which only discovery lists.
    r.write(
        "packages/core/tests/fixtures/deep.test.vlt",
        "import { expect } from \"velt/test\";\n\nexport function test_deep() {\n  expect(1).toBe(1);\n}\n",
    );
    let (ran, _) = ran_and_unchanged(&r.ok(".", &["test"]).1);
    assert_eq!(sorted(ran), ["b", "core"]);
    // `velt test -p core` runs as `velt test` in `core` and keys its stamp so: the workspace
    // run, and `-p core` again, find it unchanged.
    r.write(
        "packages/core/tests/fixtures/deep.test.vlt",
        "import { expect } from \"velt/test\";\n\nexport function test_deep() {\n  expect(2).toBe(2);\n}\n",
    );
    let (ran, _) = ran_and_unchanged(&r.ok(".", &["test", "-p", "core"]).1);
    assert_eq!(ran, ["core"]);
    let (_, unchanged) = ran_and_unchanged(&r.ok(".", &["test", "-p", "core"]).1);
    assert_eq!(unchanged, ["core"]);
    let (ran, unchanged) = ran_and_unchanged(&r.ok(".", &["test"]).1);
    assert_eq!(
        (ran, sorted(unchanged)),
        (vec!["b".to_string()], vec!["a".into(), "core".into()])
    );
    // A failing member always runs.
    r.write(
        "apps/a/tests/more.test.vlt",
        "import { expect } from \"velt/test\";\n\nexport function test_more() {\n  expect(1).toBe(2);\n}\n",
    );
    for _ in 0..2 {
        let o = r.velt(".", &["test"], &[]);
        let (ran, _) = ran_and_unchanged(&text(&o.stderr));
        assert!(ran.contains(&"a".to_string()), "{}", text(&o.stderr));
    }
}
