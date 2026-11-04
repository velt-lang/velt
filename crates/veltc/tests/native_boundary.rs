//! Who native code's permission reaches (docs/internals/design/c-ffi.md "The trust boundary"):
//! `allowNative` is about one package each, so a dependency named like the root isn't the root,
//! and an npm package under a package's directory isn't that package.

use std::path::Path;
use std::process::{Command, Output};

mod no_window;
mod test_dir;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A package.vlt with `fields` after the name and version.
fn manifest(root: &Path, rel: &str, name: &str, fields: &str) {
    write(
        root,
        &format!("{rel}/package.vlt"),
        &format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"{name}\",\n  version: \"1.0.0\",\n  {fields}\n}};\n"),
    );
}

fn velt(cwd: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Runs `args` in `cwd`, which must fail, and gives what it printed.
fn refused(cwd: &Path, args: &[&str]) -> String {
    let o = velt(cwd, args);
    let out = text(&o);
    assert!(!o.status.success(), "velt {args:?} passed: {out}");
    out
}

const C_CALL: &str = r#"declare c("c") function getpid(): i32;

export function pid(): i32 {
  // SAFETY: `getpid` takes nothing and always succeeds.
  return unsafe { getpid() };
}
"#;

const CALLS_PID: &str =
    "import { pid } from \"x\";\n\nfunction main() {\n  console.log(pid() > 0);\n}\n";

/// `app` (no `allowNative`) depends on `mid`, which depends on a package in `evil` named `app`.
fn named_like_the_root(root: &Path, evil_fields: &str) {
    manifest(
        root,
        "app",
        "app",
        "dependencies: { mid: { path: \"../mid\" } },",
    );
    write(
        root,
        "app/src/main.vlt",
        "import { pid } from \"mid\";\n\nfunction main() {\n  console.log(pid() > 0);\n}\n",
    );
    manifest(
        root,
        "mid",
        "mid",
        "dependencies: { app: { path: \"../evil\" } },",
    );
    write(root, "mid/src/lib.vlt", "export { pid } from \"app\";\n");
    manifest(root, "evil", "app", evil_fields);
    write(root, "evil/src/lib.vlt", C_CALL);
}

/// With and without native code, install and compile both stop at the name, before any of the
/// other package is built.
#[test]
fn a_dependency_can_t_take_the_root_s_name() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    named_like_the_root(root, "native: { c: {} },");
    let app = root.join("app");
    for args in [&["install"][..], &["check"], &["run"]] {
        let out = refused(&app, args);
        assert!(
            out.contains("`mid` depends on a package named `app`"),
            "{out}"
        );
        assert!(out.contains("is the name of this package"), "{out}");
    }
    named_like_the_root(root, "");
    let out = refused(&app, &["check"]);
    assert!(out.contains("is the name of this package"), "{out}");
    assert!(!app.join("velt.lock.json").exists());
}

/// The impostor's build script would run at install; it must not get that far.
#[test]
fn a_dependency_named_like_the_root_builds_nothing() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    named_like_the_root(root, "native: {},");
    let marker = root.join("build-script-ran");
    write(
        root,
        "evil/native/Cargo.toml",
        "[package]\nname = \"evilcrate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"cdylib\", \"staticlib\"]\n\n[workspace]\n",
    );
    write(root, "evil/native/src/lib.rs", "");
    write(
        root,
        "evil/native/build.rs",
        &format!(
            "fn main() {{ std::fs::write({:?}, \"\").unwrap(); }}\n",
            marker.display().to_string()
        ),
    );
    let out = refused(&root.join("app"), &["run"]);
    assert!(out.contains("is the name of this package"), "{out}");
    assert!(!marker.exists(), "the build script ran: {out}");
}

/// A package may depend back on the root itself: that's the root, and its native code is its own.
#[test]
fn a_dependency_may_depend_back_on_the_root() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    manifest(
        root,
        "app",
        "app",
        "native: { c: {} },\n  dependencies: { mid: { path: \"../mid\" } },",
    );
    write(root, "app/src/lib.vlt", C_CALL);
    write(
        root,
        "app/src/main.vlt",
        "import { name } from \"mid\";\n\nfunction main() {\n  console.log(name());\n}\n",
    );
    manifest(
        root,
        "mid",
        "mid",
        "dependencies: { app: { path: \"../app\" } },",
    );
    write(
        root,
        "mid/src/lib.vlt",
        "export function name(): string {\n  return \"mid\";\n}\n",
    );
    let o = velt(&root.join("app"), &["run"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "mid");
}

const NPM_C_CALL: &str =
    "declare c(\"c\") function getpid(): i32;\n\nexport function pid(): i32 {\n  return 1;\n}\n";

/// `good/node_modules/x`, under a dependency that is allowed.
#[test]
fn an_npm_package_in_an_allowed_package_is_not_that_package() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    manifest(root, "good", "good", "native: { c: {} },");
    write(root, "good/src/lib.vlt", "export { pid } from \"x\";\n");
    write(
        root,
        "good/node_modules/x/package.json",
        "{ \"name\": \"x\", \"version\": \"1.0.0\", \"main\": \"src/index.ts\" }",
    );
    write(root, "good/node_modules/x/src/index.ts", NPM_C_CALL);
    manifest(
        root,
        "app",
        "app",
        "allowNative: [\"good\"],\n  dependencies: { good: { path: \"../good\" } },",
    );
    write(
        root,
        "app/src/main.vlt",
        &CALLS_PID.replace("\"x\"", "\"good\""),
    );
    let out = refused(&root.join("app"), &["check"]);
    assert!(
        out.contains("this file is in an npm package under `good`"),
        "{out}"
    );
}

/// `app/node_modules/evil`, when the root itself has native code.
#[test]
fn an_npm_package_in_the_root_is_not_the_root() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    manifest(root, "app", "app", "native: { c: {} },");
    write(
        root,
        "app/src/main.vlt",
        "import { pid } from \"evil\";\n\nfunction main() {\n  console.log(pid() > 0);\n}\n",
    );
    write(
        root,
        "app/node_modules/evil/package.json",
        "{ \"name\": \"evil\", \"version\": \"1.0.0\", \"main\": \"src/index.ts\" }",
    );
    write(root, "app/node_modules/evil/src/index.ts", NPM_C_CALL);
    let out = refused(&root.join("app"), &["check"]);
    assert!(
        out.contains("this file is in an npm package under `app`"),
        "{out}"
    );
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", dir.join("no-global-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", text(&o));
}

/// `good` (allowed) at a repository's root and an npm package at its `js/`, both git
/// dependencies: the npm package is exempt from the install's check, as it has no manifest, so
/// the compiler's check has to hold.
#[test]
fn an_npm_package_in_an_allowed_git_package_is_not_that_package() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let repo = root.join("repo");
    manifest(root, "repo", "good", "native: { c: {} },");
    write(root, "repo/src/lib.vlt", "export const v = 1;\n");
    write(
        root,
        "repo/js/package.json",
        "{ \"name\": \"jsx\", \"version\": \"1.0.0\", \"main\": \"src/index.ts\" }",
    );
    write(root, "repo/js/src/index.ts", NPM_C_CALL);
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=a@b",
            "-c",
            "user.name=a",
            "commit",
            "-q",
            "-m",
            "x",
        ],
    );
    let path = repo.canonicalize().unwrap();
    let path = path.to_string_lossy().replace('\\', "/");
    let url = match path.starts_with('/') {
        true => format!("file://{path}"),
        false => format!("file:///{path}"),
    };
    manifest(
        root,
        "app",
        "app",
        &format!(
            "allowNative: [\"good\"],\n  dependencies: {{ good: {{ git: \"{url}\" }}, jsx: {{ git: \"{url}\", path: \"js\" }} }},"
        ),
    );
    write(
        root,
        "app/src/main.vlt",
        "import { pid } from \"jsx\";\n\nfunction main() {\n  console.log(pid() > 0);\n}\n",
    );
    let out = refused(&root.join("app"), &["check"]);
    assert!(
        out.contains("`declare c function getpid` is only allowed in a package"),
        "{out}"
    );
}
