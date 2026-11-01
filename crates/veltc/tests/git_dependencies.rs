//! Git dependencies through the `velt` binary (#1860), in the shape of sigx: a repository that is
//! a pnpm workspace of TypeScript packages, with a Velt package among them that imports its
//! siblings. An app depends on the Velt package by `git` and `path`, and builds without anything
//! installed into `node_modules`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod no_window;
mod runtime_support;
mod test_dir;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
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
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn velt(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    velt_with(home, cwd, args, &[])
}

fn velt_with(home: &Path, cwd: &Path, args: &[&str], env: &[(&str, &Path)]) -> Output {
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(args)
        .current_dir(cwd)
        .env("VELT_HOME", home)
        .env_remove("VELT_REGISTRY")
        .env_remove("VELT_LAUNCHER");
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.output().unwrap()
}

/// `file:///…` for `path`, with `/` separators (`file:///C:/…` on Windows), so it can be written
/// in a string literal of a manifest.
fn file_url(path: &Path) -> String {
    let path = path.display().to_string().replace('\\', "/");
    match path.starts_with('/') {
        true => format!("file://{path}"),
        false => format!("file:///{path}"),
    }
}

/// An app depending on the `demo` package of `repo` at its main branch.
fn app(root: &Path, repo: &Path) -> PathBuf {
    let app = root.join("app");
    write(
        &app,
        "package.vlt",
        &format!(
            "export const pkg: Package = {{ name: \"app\", version: \"0.1.0\", dependencies: {{ demo: {{ git: \"{}\", branch: \"main\", path: \"packages/velt\" }} }} }};\n",
            file_url(repo)
        ),
    );
    write(
        &app,
        "src/main.vlt",
        "import { hello } from \"demo\";\n\nconsole.log(hello(\"velt\"));\n",
    );
    app
}

/// The upstream repository: `packages/signals` (TypeScript, `@demo/signals`) and
/// `packages/velt` (the Velt package `demo`, which imports `@demo/signals`).
fn upstream(root: &Path, greeting: &str) -> PathBuf {
    let repo = root.join("upstream");
    write(
        &repo,
        "pnpm-workspace.yaml",
        "packages:\n  - \"packages/*\"\n",
    );
    write(
        &repo,
        "package.json",
        "{ \"name\": \"demo-monorepo\", \"private\": true }\n",
    );
    write(
        &repo,
        "packages/signals/package.json",
        "{ \"name\": \"@demo/signals\", \"exports\": { \".\": { \"types\": \"./dist/index.d.ts\", \"import\": \"./dist/index.js\" } } }\n",
    );
    write(
        &repo,
        "packages/signals/src/index.ts",
        &format!("export function greeting(who: string): string {{\n  return `{greeting} ${{who}}`;\n}}\n"),
    );
    write(
        &repo,
        "packages/velt/package.vlt",
        "export const pkg: Package = { name: \"demo\", version: \"0.1.0\" };\n",
    );
    write(
        &repo,
        "packages/velt/src/lib.vlt",
        "import { greeting } from \"@demo/signals\";\n\nexport function hello(who: string): string {\n  return greeting(who);\n}\n",
    );
    if !repo.join(".git").exists() {
        git(&repo, &["init", "-q", "-b", "main"]);
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", greeting]);
    repo
}

#[test]
fn an_app_builds_against_a_package_of_a_git_monorepo() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let root = tmp.path();
    let home = root.join("home");
    let repo = upstream(root, "hello");
    let app = app(root, &repo);
    let run = || {
        let o = velt(&home, &app, &["run"]);
        let err = String::from_utf8_lossy(&o.stderr).into_owned();
        assert!(o.status.success(), "{err}");
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    assert_eq!(run(), "hello velt\n");
    let lock = std::fs::read_to_string(app.join("velt.lock.json")).unwrap();
    let head = git(&repo, &["rev-parse", "HEAD"]);
    assert!(
        lock.contains(&format!("?branch=main&path=packages/velt#{head}")),
        "{lock}"
    );

    // Upstream moves on: the build stays on the locked commit until `velt update`.
    upstream(root, "hi");
    assert_eq!(run(), "hello velt\n");
    let o = velt(&home, &app, &["update"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(run(), "hi velt\n");
}

/// The checkout, and so the lock's checksum, is the same whatever the machine's git configuration
/// does to line endings (Git for Windows sets `core.autocrlf`), and the variables a git hook sets
/// never send velt's git commands into the user's repository.
#[test]
fn git_configuration_and_hooks_change_nothing() {
    let tmp = test_dir::TestDir::new();
    let root = tmp.path();
    let home = root.join("home");
    let repo = upstream(root, "hello");
    let app = app(root, &repo);
    let o = velt(&home, &app, &["install"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // As on a machine whose configuration converts line endings: the lock still matches.
    std::fs::remove_dir_all(home.join("cache/git")).unwrap();
    let config = root.join("crlf.gitconfig");
    std::fs::write(&config, "[core]\n\tautocrlf = true\n").unwrap();
    let o = velt_with(
        &home,
        &app,
        &["install", "--locked"],
        &[("GIT_CONFIG_GLOBAL", &config)],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // As inside a `pre-commit` hook of another repository.
    let victim = root.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    git(&victim, &["init", "-q"]);
    write(&victim, "mine.txt", "mine\n");
    git(&victim, &["add", "mine.txt"]);
    let objects = |dir: &Path| walk(&dir.join(".git/objects"));
    let before = objects(&victim);
    std::fs::remove_dir_all(home.join("cache/git")).unwrap();
    let git_dir = victim.join(".git");
    let index = git_dir.join("index");
    let env = [
        ("GIT_DIR", git_dir.as_path()),
        ("GIT_INDEX_FILE", &index),
        ("GIT_WORK_TREE", &victim),
    ];
    let o = velt_with(&home, &app, &["install", "--locked"], &env);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(objects(&victim), before);
    assert_eq!(git(&victim, &["status", "--porcelain"]), "A  mine.txt");
}

/// The files below `dir`, recursively.
fn walk(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| {
                    if e.path().is_dir() {
                        walk(&e.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

/// An npm package of a git monorepo, imported directly: no Velt package in between, nothing in
/// `node_modules`; and the same dependency written once in a workspace for its members.
#[test]
fn npm_packages_come_straight_from_a_git_monorepo() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let root = tmp.path();
    let home = root.join("home");
    let repo = upstream(root, "hello");
    let url = file_url(&repo);
    let main = "import { greeting } from \"@demo/signals\";\n\nconsole.log(greeting(\"npm\"));\n";

    let app = root.join("app");
    write(
        &app,
        "package.vlt",
        &format!("export const pkg: Package = {{ name: \"app\", version: \"0.1.0\", dependencies: {{ \"@demo/signals\": {{ git: \"{url}\" }} }} }};\n"),
    );
    write(&app, "src/main.vlt", main);
    let o = velt(&home, &app, &["run"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "hello npm\n");
    assert!(!app.join("node_modules").exists());

    // In a workspace: written once, used by a member as `"workspace"`.
    let ws = root.join("ws");
    write(
        &ws,
        "velt.workspace.vlt",
        &format!("export const workspace: Workspace = {{ members: [\"app\"], dependencies: {{ \"@demo/signals\": {{ git: \"{url}\", branch: \"main\" }} }} }};\n"),
    );
    write(
        &ws,
        "app/package.vlt",
        "export const pkg: Package = { name: \"app\", version: \"0.1.0\", dependencies: { \"@demo/signals\": \"workspace\" } };\n",
    );
    write(&ws, "app/src/main.vlt", main);
    let o = velt(&home, &ws.join("app"), &["run"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "hello npm\n");
    let lock = std::fs::read_to_string(ws.join("velt.lock.json")).unwrap();
    assert!(lock.contains("?branch=main#"), "{lock}");
}
