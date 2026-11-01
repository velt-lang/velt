//! Workspaces through the `velt` binary (docs/internals/design/workspaces.md): members that
//! depend on each other with `"workspace"`, one lockfile at the root, and builds that keep each
//! member's program and build stamp apart, wherever `velt` runs from, while the workspace's own
//! files are inputs of every member's stamp.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

struct Repo {
    _tmp: test_dir::TestDir,
    dir: PathBuf,
}

fn repo() -> Repo {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path().to_path_buf();
    let repo = Repo { _tmp: tmp, dir };
    repo.write(
        "velt.workspace.vlt",
        "export const workspace: Workspace = { members: [\"packages/*\", \"apps/*\"] };\n",
    );
    repo.package("packages/core", "core", "");
    repo.write(
        "packages/core/src/lib.vlt",
        "export function greet(who: string): string {\n  return `hello ${who}`;\n}\n",
    );
    for app in ["a", "b"] {
        repo.package(&format!("apps/{app}"), app, "core: \"workspace\"");
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

    fn package(&self, rel: &str, name: &str, deps: &str) {
        self.write(
            &format!("{rel}/package.vlt"),
            &format!(
                "export const pkg: Package = {{ name: \"{name}\", version: \"0.1.0\", dependencies: {{ {deps} }} }};\n"
            ),
        );
    }

    fn velt(&self, cwd: &str, args: &[&str]) -> Output {
        crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(args)
            .current_dir(self.dir.join(cwd))
            .env("VELT_HOME", self.dir.join("home"))
            .env_remove("VELT_REGISTRY")
            .env_remove("VELT_LAUNCHER")
            .output()
            .unwrap()
    }

    fn ok(&self, cwd: &str, args: &[&str]) -> (String, String) {
        let o = self.velt(cwd, args);
        let (out, err) = (text(&o.stdout), text(&o.stderr));
        assert!(
            o.status.success(),
            "`velt {}` in {cwd}:\n{err}{out}",
            args.join(" ")
        );
        (out, err)
    }

    /// Whether `velt build -v` of `input` from `cwd` compiled nothing (its stamp hit).
    fn up_to_date(&self, cwd: &str, input: &str) -> bool {
        self.ok(cwd, &["build", input, "-v"])
            .1
            .contains("nothing to compile")
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

#[test]
fn members_share_a_lock_and_keep_their_build_stamps_apart() {
    let r = repo();
    assert_eq!(r.ok("apps/a", &["run"]).0, "hello a\n");
    assert!(r.dir.join("velt.lock.json").is_file());
    assert!(!r.dir.join("apps/a/velt.lock.json").exists());

    // Each member's program is in its own `target/`, so the two `src/main.vlt` never share a
    // stamp: building one leaves the other up to date.
    assert!(!r.up_to_date("apps/a", "src/main.vlt"));
    assert!(r.up_to_date("apps/a", "src/main.vlt"));
    assert!(!r.up_to_date("apps/b", "src/main.vlt"));
    assert!(r.up_to_date("apps/a", "src/main.vlt"));
    assert!(r.dir.join("apps/b/target/velt").join(exe("main")).is_file());

    // From the workspace root, a member builds into its own `target/` and hits its stamp.
    assert!(r.up_to_date(".", "apps/b/src/main.vlt"));
    assert!(!r.dir.join("target").exists());

    // A member another one uses is an input of both.
    r.write(
        "packages/core/src/lib.vlt",
        "export function greet(who: string): string {\n  return `hi ${who}`;\n}\n",
    );
    assert!(!r.up_to_date("apps/a", "src/main.vlt"));
    assert_eq!(r.ok("apps/b", &["run"]).0, "hi b\n");
}

#[test]
fn the_workspace_s_files_are_inputs_of_every_member_s_stamp() {
    let r = repo();
    assert!(!r.up_to_date("apps/a", "src/main.vlt"));
    assert!(r.up_to_date("apps/a", "src/main.vlt"));
    // A new member that a glob lists changes the workspace's resolution and lock.
    r.package("packages/extra", "extra", "");
    r.write("packages/extra/src/lib.vlt", "export const x = 1;\n");
    assert!(!r.up_to_date("apps/a", "src/main.vlt"));
    assert!(r.up_to_date("apps/a", "src/main.vlt"));
    // So does the workspace file.
    r.write(
        "velt.workspace.vlt",
        "export const workspace: Workspace = { members: [\"packages/*\", \"apps/a\"] };\n",
    );
    assert!(!r.up_to_date("apps/a", "src/main.vlt"));
}

#[test]
fn install_at_the_root_installs_every_member() {
    let r = repo();
    let (_, err) = r.ok(".", &["install"]);
    assert!(err.contains("Installed 3 packages"), "{err}");
    let lock = std::fs::read_to_string(r.dir.join("velt.lock.json")).unwrap();
    assert!(
        lock.contains("\"source\": \"workspace+packages/core\""),
        "{lock}"
    );

    // A member written as a path dependency is an error that says what to write.
    r.package("apps/b", "b", "core: { path: \"../../packages/core\" }");
    let o = r.velt("apps/b", &["run"]);
    assert!(!o.status.success());
    let err = text(&o.stderr);
    assert!(
        err.contains("apps/b/package.vlt:1:")
            && err.contains(
                "error: `core` is a member of this workspace; write `core: \"workspace\"`"
            ),
        "{err}"
    );
}
