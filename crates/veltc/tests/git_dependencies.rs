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
    crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(cwd)
        .env("VELT_HOME", home)
        .env_remove("VELT_REGISTRY")
        .env_remove("VELT_LAUNCHER")
        .output()
        .unwrap()
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
    let app = root.join("app");
    write(
        &app,
        "package.vlt",
        &format!(
            "export const pkg: Package = {{ name: \"app\", version: \"0.1.0\", dependencies: {{ demo: {{ git: \"file://{}\", branch: \"main\", path: \"packages/velt\" }} }} }};\n",
            repo.display()
        ),
    );
    write(
        &app,
        "src/main.vlt",
        "import { hello } from \"demo\";\n\nconsole.log(hello(\"velt\"));\n",
    );
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
