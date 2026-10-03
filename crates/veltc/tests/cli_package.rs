//! CLI-level package workflows through the `velt` binary, with an isolated `VELT_HOME` per test:
//! `new` → `publish` → `add` → `install` → lockfile, plus `build`/`run`/`test` in package mode.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod test_dir;

struct Sandbox {
    _tmp: test_dir::TestDir,
    dir: PathBuf,
}

fn sandbox() -> Sandbox {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path().to_path_buf();
    Sandbox { _tmp: tmp, dir }
}

impl Sandbox {
    fn velt(&self, cwd: &str, args: &[&str]) -> Output {
        let cwd = self.dir.join(cwd);
        Command::new(env!("CARGO_BIN_EXE_velt"))
            .args(args)
            .current_dir(&cwd)
            .env("VELT_HOME", self.dir.join("home"))
            .env_remove("VELT_REGISTRY")
            .output()
            .unwrap()
    }

    /// Run and assert success; returns stderr (status lines).
    fn ok(&self, cwd: &str, args: &[&str]) -> String {
        let o = self.velt(cwd, args);
        let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
        assert!(
            o.status.success(),
            "`velt {}` failed:\n{stderr}{}",
            args.join(" "),
            String::from_utf8_lossy(&o.stdout)
        );
        stderr
    }

    fn fail(&self, cwd: &str, args: &[&str]) -> String {
        let o = self.velt(cwd, args);
        assert!(!o.status.success(), "`velt {}` should fail", args.join(" "));
        String::from_utf8_lossy(&o.stderr).into_owned()
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.join(rel)).unwrap()
    }

    fn write(&self, rel: &str, text: &str) {
        std::fs::write(self.dir.join(rel), text).unwrap();
    }
}

fn set_version(s: &Sandbox, pkg: &str, version: &str) {
    let manifest = s
        .read(&format!("{pkg}/package.vlt"))
        .replace("0.1.0", version)
        .replace("1.0.0", version);
    s.write(&format!("{pkg}/package.vlt"), &manifest);
}

#[test]
fn new_publish_add_install() {
    let s = sandbox();
    s.ok("", &["new", "mylib", "--lib"]);
    set_version(&s, "mylib", "1.0.0");
    assert!(s
        .ok("mylib", &["publish"])
        .contains("Published `mylib` 1.0.0"));
    set_version(&s, "mylib", "1.2.0");
    s.ok("mylib/src", &["publish"]); // found by searching upward
    assert!(s.fail("mylib", &["publish"]).contains("already published"));
    assert!(s.dir.join("home/registry/mylib/index.json").is_file());

    s.ok("", &["new", "app"]);
    s.ok("app", &["add", "mylib@^1.0"]);
    let manifest = s.read("app/package.vlt");
    assert!(manifest.contains("mylib: \"^1.0\""), "{manifest}");
    let lock = s.read("app/velt.lock.json");
    assert!(
        lock.contains("\"name\": \"mylib\"") && lock.contains("\"version\": \"1.2.0\""),
        "{lock}"
    );
    assert!(s.dir.join("home/cache/mylib-1.2.0/src/lib.vlt").is_file());
    s.ok("app", &["install", "--locked"]);

    // Unknown package: error, and package.vlt is left untouched.
    assert!(s
        .fail("app", &["add", "nope"])
        .contains("not in the registry"));
    assert_eq!(s.read("app/package.vlt"), manifest);
    // Conflict: a requirement nothing satisfies.
    assert!(s
        .fail("app", &["add", "mylib@^2"])
        .contains("no version of `mylib` matches"));
    assert_eq!(s.read("app/package.vlt"), manifest);
}

#[test]
fn add_without_version_uses_latest_and_path_deps_work() {
    let s = sandbox();
    s.ok("", &["new", "util", "--lib"]);
    s.ok("util", &["publish"]);
    s.ok("", &["new", "app"]);
    s.ok("app", &["add", "util"]);
    assert!(s.read("app/package.vlt").contains("util: \"0.1.0\""));
    // A package with only pre-releases: `add` picks the newest, the version `search` shows.
    s.ok("", &["new", "beta", "--lib"]);
    set_version(&s, "beta", "0.2.0-beta.1");
    s.ok("beta", &["publish"]);
    let found = s.velt("app", &["search", "beta"]);
    assert!(String::from_utf8_lossy(&found.stdout).contains("beta  0.2.0-beta.1"));
    s.ok("app", &["add", "beta"]);
    assert!(s.read("app/package.vlt").contains("beta: \"0.2.0-beta.1\""));

    s.ok("", &["new", "local", "--lib"]);
    s.ok("app", &["add", "local", "--path", "../local"]);
    assert!(s
        .read("app/package.vlt")
        .contains("local: { path: \"../local\" }"));
    assert!(s
        .read("app/velt.lock.json")
        .contains("\"source\": \"path+../local\""));
}

#[test]
fn package_commands_outside_a_package() {
    let s = sandbox();
    assert!(s.fail("", &["install"]).contains("no `package.vlt`"));
    assert!(s.fail("", &["build"]).contains("no `package.vlt`"));
    assert!(s.fail("", &["new", "Bad"]).contains("invalid package name"));
    s.ok("", &["new", "lib", "--lib"]);
    assert!(s.fail("lib", &["build"]).contains("it is a library"));
}

#[test]
fn import_of_undeclared_package_is_reported() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    s.write(
        "app/src/main.vlt",
        "import { f } from \"json\";\nfunction main() {}\n",
    );
    let err = s.fail("app", &["build"]);
    assert!(
        err.contains("package `json` is not a dependency (add it with `velt add json`)"),
        "{err}"
    );
}

/// Needs the full compiler pipeline (VIR lowering + codegen).
#[test]
fn new_then_build_and_run() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    s.ok("app", &["build"]);
    let exe = if cfg!(windows) {
        "app/target/velt/app.exe"
    } else {
        "app/target/velt/app"
    };
    assert!(s.dir.join(exe).is_file());
    let o = s.velt("app", &["run"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "Hello, world!\n");
}

/// Needs the full compiler pipeline and the prelude's `assert`.
#[test]
fn test_runner_reports_passes_and_failures() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    s.write(
        "app/src/math.test.vlt",
        "export function test_ok() { assert(1 + 1 == 2); }\n\
         export function test_bad() { assertEq(1, 2); }\n\
         export function test_after() { console.log(\"still runs\"); }\n",
    );
    let o = s.velt("app", &["test"]);
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(1), "{stdout}");
    for want in [
        "ok test_ok",
        "FAILED test_bad",
        "still runs",
        "ok test_after",
        "4 passed; 1 failed", // + the template's two tests in tests/greet.test.vlt
    ] {
        assert!(stdout.contains(want), "missing `{want}` in:\n{stdout}");
    }
    assert!(Path::new(&s.dir.join("app/target/velt/test")).is_dir());
}

#[test]
fn manifest_json_prints_the_validated_manifest() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    s.ok("", &["new", "util", "--lib"]);
    s.ok("app", &["add", "util", "--path", "../util"]);
    let out = s.velt("app/src", &["manifest", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["name"], "app");
    assert_eq!(json["entry"], "src/main.vlt");
    assert_eq!(json["dependencies"]["util"]["path"], "../util");

    s.write(
        "app/package.vlt",
        "export const pkg: Package = { name: \"app\" };\n",
    );
    let err = s.fail("app", &["manifest", "--json"]);
    assert!(
        err.contains("package.vlt:1:29: error: the manifest is missing `version`"),
        "{err}"
    );
}

#[test]
fn a_velt_toml_is_reported_with_its_conversion() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    std::fs::remove_file(s.dir.join("app/package.vlt")).unwrap();
    s.write(
        "app/velt.toml",
        "[package]\nname = \"app\"\nversion = \"0.2.0\"\n\n[paths]\n\"@app/*\" = \"src/*\"\n",
    );
    let err = s.fail("app", &["build"]);
    assert!(
        err.contains("is no longer read; the manifest is `package.vlt`"),
        "{err}"
    );
    let converted = &err[err.find("import type").expect("the converted manifest")..];
    assert!(converted.contains("\"@app/*\": \"src/*\""), "{converted}");
    // Saving the printed file is all the migration takes.
    s.write("app/package.vlt", converted);
    s.ok("app", &["build"]);
}

#[test]
fn manifest_checks_and_every_command_reports_the_same_error() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    let checked = s.ok("app/src", &["manifest"]);
    assert!(
        checked.contains("Checked") && checked.contains("(app 0.1.0)"),
        "{checked}"
    );
    s.write(
        "app/package.vlt",
        "export const pkg: Package = { name: \"app\", version: \"0.1.0\", deps: {} };\n",
    );
    for args in [
        &["manifest"][..],
        &["manifest", "--json"],
        &["build"],
        &["install"],
    ] {
        let err = s.fail("app", args);
        assert!(
            err.contains("package.vlt:1:62: error: unknown key `deps` in the manifest"),
            "velt {}: {err}",
            args.join(" ")
        );
    }
}

#[test]
fn fmt_reports_a_velt_toml_like_every_command() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    std::fs::remove_file(s.dir.join("app/package.vlt")).unwrap();
    s.write(
        "app/velt.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    let err = s.fail("app", &["fmt"]);
    assert!(
        err.contains("is no longer read; the manifest is `package.vlt`"),
        "{err}"
    );
}
