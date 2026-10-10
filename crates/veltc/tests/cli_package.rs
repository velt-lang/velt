//! CLI-level package workflows through the `velt` binary, with an isolated `VELT_HOME` per test:
//! `new` → `publish` → `add` → `install` → lockfile, plus `build`/`run`/`test` in package mode.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

struct Sandbox {
    _tmp: test_dir::TestDir,
    dir: PathBuf,
}

fn sandbox() -> Sandbox {
    // `velt build` / `run` / `test` link programs against the runtime libraries.
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path().to_path_buf();
    Sandbox { _tmp: tmp, dir }
}

impl Sandbox {
    fn velt(&self, cwd: &str, args: &[&str]) -> Output {
        let cwd = self.dir.join(cwd);
        crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
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
fn check_in_a_library_package_checks_src_lib() {
    let s = sandbox();
    s.ok("", &["new", "util", "--lib"]);
    let o = s.velt("util", &["check"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    s.write(
        "util/src/lib.vlt",
        "export function f(): i64 {\n  return \"x\";\n}\n",
    );
    let err = s.fail("util", &["check"]);
    assert!(err.contains("lib.vlt:2:"), "{err}");
    std::fs::remove_file(s.dir.join("util/src/lib.vlt")).unwrap();
    let err = s.fail("util", &["check"]);
    assert!(
        err.contains("has neither `src/main.vlt` nor `src/lib.vlt`"),
        "{err}"
    );
    // A configured entry that is missing is an error, even when `src/lib.vlt` exists.
    s.write(
        "util/src/lib.vlt",
        "export function f(): i64 {\n  return 1;\n}\n",
    );
    let manifest = std::fs::read_to_string(s.dir.join("util/package.vlt")).unwrap();
    let manifest = manifest.replacen(
        "version: \"0.1.0\"",
        "version: \"0.1.0\", entry: \"src/app.vlt\"",
        1,
    );
    s.write("util/package.vlt", &manifest);
    let err = s.fail("util", &["check"]);
    assert!(err.contains("has no `src/app.vlt` to check"), "{err}");
}

/// `fn(): i64` returning a string: one type error at line 2 of the file.
const BAD: &str = "export function bad(): i64 {\n  return \"x\";\n}\n";

#[test]
fn check_in_a_package_checks_every_module_under_src_and_tests() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    s.ok("app", &["check"]);
    // Nothing imports these, so only checking the whole package finds their errors.
    std::fs::create_dir_all(s.dir.join("app/src/util")).unwrap();
    s.write("app/src/lib.vlt", BAD);
    s.write("app/src/util/unused.vlt", BAD);
    s.write("app/tests/lib.test.vlt", BAD);
    let err = s.fail("app", &["check"]);
    for file in ["lib.vlt:2:", "unused.vlt:2:", "lib.test.vlt:2:"] {
        assert!(err.contains(file), "missing `{file}` in:\n{err}");
    }
    let out = s.velt("app", &["check", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["errors"], 3, "{json}");
    // `velt check <file>` still checks only that file and its imports.
    s.ok("app", &["check", "src/main.vlt"]);

    // A module both the entry and the library import is checked, and reported, once.
    for file in ["src/lib.vlt", "src/util/unused.vlt", "tests/lib.test.vlt"] {
        std::fs::remove_file(s.dir.join("app").join(file)).unwrap();
    }
    s.write("app/src/shared.vlt", BAD);
    s.write("app/src/lib.vlt", "import { bad } from \"./shared\";\n");
    let main = s.read("app/src/main.vlt");
    s.write(
        "app/src/main.vlt",
        &format!("import {{ bad }} from \"./shared\";\n{main}"),
    );
    let err = s.fail("app", &["check"]);
    assert_eq!(err.matches("shared.vlt:2:").count(), 1, "{err}");
}

#[test]
fn check_validates_the_entry_main_but_not_other_modules() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    s.write("app/src/helper.vlt", "export const main: i64 = 1;\n");
    s.ok("app", &["check"]);
    s.write("app/src/main.vlt", "export function f() {}\n");
    let err = s.fail("app", &["check"]);
    assert!(err.contains("`main` function not found"), "{err}");
}

#[test]
fn check_accepts_an_unimported_main_next_to_a_custom_entry() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    let manifest = s.read("app/package.vlt").replacen(
        "version: \"0.1.0\"",
        "version: \"0.1.0\", entry: \"src/app.vlt\"",
        1,
    );
    s.write("app/package.vlt", &manifest);
    s.write("app/src/app.vlt", "function main() {}\n");
    // `src/main.vlt` (from `velt new`) and `src/main/index.vlt` would both be module `main`.
    std::fs::create_dir_all(s.dir.join("app/src/main")).unwrap();
    s.write("app/src/main/index.vlt", "export function f() {}\n");
    s.ok("app", &["build"]);
    s.ok("app", &["check"]);
    s.write("app/src/main.vlt", BAD);
    let err = s.fail("app", &["check"]);
    let at = format!("{}:2:", Path::new("src").join("main.vlt").display());
    assert!(err.contains(&at), "missing `{at}` in:\n{err}");
}

#[test]
fn check_accepts_a_module_named_like_a_dependency() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    s.ok("", &["new", "util", "--lib"]);
    s.write(
        "util/src/lib.vlt",
        "export function u(): i64 {\n  return 1;\n}\n",
    );
    s.ok("app", &["add", "util", "--path", "../util"]);
    s.write("app/src/util.vlt", "export function local() {}\n");
    s.write(
        "app/tests/u.test.vlt",
        "import { u } from \"util\";\nexport function test_u() {\n  assertEq(u(), 1);\n}\n",
    );
    s.write(
        "app/src/main.vlt",
        "import { u } from \"util\";\nfunction main() {\n  console.log(`${u()}`);\n}\n",
    );
    s.ok("app", &["build"]);
    s.ok("app", &["check"]);
    // Without the entry importing it, `src/util.vlt` is loaded before the test's `util`.
    s.write("app/src/main.vlt", "function main() {}\n");
    s.ok("app", &["check"]);
    s.ok("app", &["test"]);
}

#[test]
fn check_checks_modules_under_a_std_directory() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    std::fs::create_dir_all(s.dir.join("app/src/std")).unwrap();
    s.write("app/src/std/x.vlt", "export function x() {}\n");
    s.ok("app", &["build"]);
    s.ok("app", &["check"]);
    s.write("app/src/std/x.vlt", BAD);
    let err = s.fail("app", &["check"]);
    let at = format!("{}:2:", Path::new("std").join("x.vlt").display());
    assert!(err.contains(&at), "missing `{at}` in:\n{err}");
}

#[test]
fn check_and_test_leave_nested_packages_alone() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    // A package inside `src/`: its manifest is not a module, and its files are its own.
    s.ok("app/src", &["new", "vendored", "--lib"]);
    s.write("app/src/vendored/src/lib.vlt", BAD);
    s.write(
        "app/src/vendored/src/lib.test.vlt",
        "export function test_fails() {\n  assertEq(1, 2);\n}\n",
    );
    s.ok("app", &["check"]);
    let out = s.ok("app", &["test"]);
    assert!(!out.contains("test_fails"), "{out}");
    // In the nested package, its files are checked.
    s.fail("app/src/vendored", &["check"]);
}

#[cfg(unix)]
#[test]
fn check_does_not_follow_symlinked_directories() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    std::os::unix::fs::symlink("..", s.dir.join("app/src/up")).unwrap();
    s.write("app/examples.vlt", BAD);
    s.ok("app", &["check"]);
}

#[test]
fn a_missing_custom_entry_is_named_without_calling_the_package_a_library() {
    let s = sandbox();
    s.ok("", &["new", "util", "--lib"]);
    let manifest = s.read("util/package.vlt").replacen(
        "version: \"0.1.0\"",
        "version: \"0.1.0\", entry: \"src/app.vlt\"",
        1,
    );
    s.write("util/package.vlt", &manifest);
    let err = s.fail("util", &["build"]);
    assert!(err.contains("has no `src/app.vlt` to build"), "{err}");
    assert!(!err.contains("library"), "{err}");
    let err = s.fail("util", &["check"]);
    assert!(err.contains("has no `src/app.vlt` to check"), "{err}");
    assert!(!err.contains("library"), "{err}");
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

#[test]
fn search_matches_descriptions_and_prints_json() {
    let s = sandbox();
    s.ok("", &["new", "textkit", "--template", "lib"]);
    let manifest = s
        .read("textkit/package.vlt")
        .replace("What textkit does, in one sentence.", "Wrap and pad text")
        .replace(
            "version: \"0.1.0\",",
            "version: \"0.1.0\",\n  keywords: [\"strings\"],",
        );
    s.write("textkit/package.vlt", &manifest);
    s.ok("textkit", &["publish"]);

    let out = s.velt("", &["search", "pad"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.trim_end(), "textkit  0.1.0  Wrap and pad text");

    let out = s.velt("", &["search", "strings", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["packages"][0]["name"], "textkit");
    assert_eq!(json["packages"][0]["description"], "Wrap and pad text");
    assert_eq!(json["packages"][0]["keywords"][0], "strings");
}

/// `.ts` and `.tsx` modules next to `.vlt` ones (#354): imported, run as the root file, tested,
/// checked with the whole package and formatted. Needs the full compiler pipeline.
#[test]
fn ts_and_tsx_modules_in_a_package() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    let manifest = s.read("app/package.vlt").replacen(
        "{ name: \"app\", ",
        "{\n  name: \"app\",\n  jsx: { importSource: \"./ui\" },\n  ",
        1,
    );
    let manifest = manifest
        .replacen(", velt:", ",\n  velt:", 1)
        .replacen(" };", ",\n};", 1);
    s.write("app/package.vlt", &manifest);
    s.write(
        "app/src/model.ts",
        "export function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n",
    );
    s.write(
        "app/src/card.tsx",
        "export function Card(props: { title: string }): JSX.Element {\n  return <h1>{props.title}</h1>;\n}\n",
    );
    s.write(
        "app/src/main.vlt",
        "import { renderToStringSync } from \"velt:jsx/render\";\nimport { Card } from \"./card\";\nimport { greet } from \"./model\";\n\nfunction main() {\n  console.log(greet(\"ts\"));\n  console.log(renderToStringSync(<Card title=\"tsx\" />));\n}\n",
    );
    // The package's JSX provider applies to `.tsx` modules.
    let err = s.fail("app", &["check"]);
    assert!(
        err.contains("card.tsx:2:10") && err.contains("from `jsx.importSource` in package.vlt"),
        "{err}"
    );
    std::fs::create_dir_all(s.dir.join("app/ui")).unwrap();
    s.write(
        "app/ui/jsx-runtime.vlt",
        "export * from \"velt:jsx/jsx-runtime\";\n",
    );
    s.ok("app", &["check"]);
    let o = s.velt("app", &["run"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "Hello, ts!\n<h1>tsx</h1>\n",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    // A `.ts` root file, and a `.test.ts` test file.
    s.write(
        "app/src/hello.ts",
        "import { greet } from \"./model\";\n\nfunction main() {\n  console.log(greet(\"root\"));\n}\n",
    );
    let o = s.velt("app", &["run", "src/hello.ts"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "Hello, root!\n");
    s.write(
        "app/tests/model.test.ts",
        "import { greet } from \"../src/model\";\n\nexport function test_greet() {\n  assertEq(greet(\"a\"), \"Hello, a!\");\n}\n",
    );
    // `velt test` runs `model.test.ts` and `model.test.vlt` side by side.
    s.write(
        "app/tests/model.test.vlt",
        "export function test_vlt() {\n  assertEq(1, 1);\n}\n",
    );
    let o = s.velt("app", &["test"]);
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        o.status.success() && stdout.contains("ok test_greet") && stdout.contains("ok test_vlt"),
        "{stdout}"
    );
    s.ok("app", &["fmt", "--check"]);
    // Whole-package `velt check` reports files with the same module path, whatever imports them.
    s.write("app/src/dup.vlt", "export function d() {}\n");
    s.write("app/src/dup.ts", "export function d() {}\n");
    let err = s.fail("app", &["check"]);
    for msg in [
        "`src/dup.ts` and `src/dup.vlt` have the same module path",
        "`tests/model.test.ts` and `tests/model.test.vlt` have the same module path",
    ] {
        assert!(err.contains(msg), "missing `{msg}` in:\n{err}");
    }
    let out = s.velt("app", &["check", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["errors"], 2, "{json}");
    for file in ["src/dup.ts", "tests/model.test.vlt"] {
        std::fs::remove_file(s.dir.join("app").join(file)).unwrap();
    }
    s.ok("app", &["check"]);

    // Whole-package `velt check` reports a `.ts` module nothing imports.
    s.write("app/src/extra.ts", BAD);
    let err = s.fail("app", &["check"]);
    assert!(err.contains("extra.ts:2:"), "{err}");
}

#[test]
fn a_package_written_in_typescript_needs_no_entry() {
    let s = sandbox();
    s.ok("", &["new", "textkit", "--lib"]);
    std::fs::remove_file(s.dir.join("textkit/src/lib.vlt")).unwrap();
    std::fs::remove_dir_all(s.dir.join("textkit/tests")).unwrap();
    s.write("textkit/src/lib.ts", "export { shout } from \"./shout\";\n");
    s.write(
        "textkit/src/shout.ts",
        "export function shout(s: string): string {\n  return `${s}!`;\n}\n",
    );
    s.ok("textkit", &["check"]);
    s.ok("", &["new", "app"]);
    s.ok("app", &["add", "textkit", "--path", "../textkit"]);
    std::fs::remove_file(s.dir.join("app/src/main.vlt")).unwrap();
    s.write(
        "app/src/main.ts",
        "import { shout } from \"textkit\";\nimport { shout as again } from \"textkit/shout\";\n\nfunction main() {\n  console.log(again(shout(\"hi\")));\n}\n",
    );
    s.ok("app", &["check"]);
    let o = s.velt("app", &["run"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "hi!!\n",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    // Two default entries are ambiguous, as two candidates of an import are.
    s.write("app/src/main.vlt", "function main() {}\n");
    let err = s.fail("app", &["run"]);
    assert!(
        err.contains("more than one `src/main` module: `src/main.vlt`, `src/main.ts`"),
        "{err}"
    );
}
