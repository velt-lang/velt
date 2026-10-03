//! Project scaffolding through the `velt` binary: every `velt new --template` project builds,
//! passes `velt test` and is formatted; `velt init` refuses to overwrite; `velt clean`; and the
//! CLI polish (help, completions, typo suggestions, colors, errors for common mistakes).

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
    fn command(&self, cwd: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_velt"));
        cmd.args(args)
            .current_dir(self.dir.join(cwd))
            .env("VELT_HOME", self.dir.join("home"))
            .env_remove("VELT_REGISTRY")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("NO_COLOR");
        cmd
    }

    fn velt(&self, cwd: &str, args: &[&str]) -> Output {
        self.command(cwd, args).output().unwrap()
    }

    /// Run and assert success; returns stdout + stderr.
    fn ok(&self, cwd: &str, args: &[&str]) -> String {
        let o = self.velt(cwd, args);
        let text = text(&o);
        assert!(
            o.status.success(),
            "`velt {}` failed:\n{text}",
            args.join(" ")
        );
        text
    }

    /// Run and assert failure; returns stdout + stderr.
    fn fail(&self, cwd: &str, args: &[&str]) -> String {
        let o = self.velt(cwd, args);
        let text = text(&o);
        assert!(
            !o.status.success(),
            "`velt {}` should fail:\n{text}",
            args.join(" ")
        );
        text
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// `velt new` with `template`, then build (binaries), test and format-check the project.
fn check_template(template: &str, has_main: bool) {
    let s = sandbox();
    let name = format!("demo-{template}");
    let out = s.ok("", &["new", &name, "--template", template]);
    assert!(out.contains(&format!("({template} template)")), "{out}");
    let root = s.path(&name);
    for file in ["package.vlt", ".gitignore", "README.md"] {
        assert!(root.join(file).is_file(), "{template}: no {file}");
    }
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    assert!(readme.starts_with(&format!("# {name}\n")), "{readme}");
    assert_eq!(root.join("src/main.vlt").is_file(), has_main, "{template}");
    if has_main {
        s.ok(&name, &["build"]);
        assert!(exe(&root.join("target/velt"), &name).is_file());
    }
    let tested = s.ok(&name, &["test"]);
    assert!(
        tested.contains("test result: ok.") && !tested.contains("0 passed"),
        "{template}: {tested}"
    );
    s.ok(&name, &["fmt", "--check", "src", "tests"]);
    // The generated package.vlt is formatted too (`velt fmt` includes it by default).
    s.ok(&name, &["fmt", "--check"]);
}

fn exe(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

#[test]
fn template_app() {
    check_template("app", true);
}

#[test]
fn template_cli() {
    check_template("cli", true);
}

#[test]
fn template_api() {
    check_template("api", true);
}

#[test]
fn template_websocket() {
    check_template("websocket", true);
}

#[test]
fn template_lib() {
    check_template("lib", false);
    // A library starts with a description to edit (it is shown by `velt search`).
    let s = sandbox();
    s.ok("", &["new", "textkit", "--template", "lib"]);
    let manifest = std::fs::read_to_string(s.path("textkit").join("package.vlt")).unwrap();
    assert!(
        manifest.contains("description: \"What textkit does, in one sentence.\""),
        "{manifest}"
    );
}

#[test]
fn default_template_runs() {
    let s = sandbox();
    s.ok("", &["new", "hello"]);
    let out = s.ok("hello", &["run", "--", "Ada"]);
    assert!(out.contains("Hello, Ada!"), "{out}");
    let err = s.fail("", &["new", "hello"]);
    assert!(
        err.contains("already exists") && err.contains("velt init"),
        "{err}"
    );
    assert!(s.fail("", &["new", "Bad"]).contains("invalid package name"));
    assert!(s
        .fail("", &["new", "x", "--template", "rest"])
        .contains("unknown template `rest`"));
}

#[test]
fn init_refuses_to_overwrite_unless_forced() {
    let s = sandbox();
    let dir = s.path("My Tool");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.vlt"), "// mine\n").unwrap();
    std::fs::write(dir.join("README.md"), "my readme\n").unwrap();
    std::fs::write(dir.join(".gitignore"), "*.log\n").unwrap();

    let err = s.fail("My Tool", &["init", "--template", "cli"]);
    assert!(
        err.contains("src/main.vlt") && err.contains("--force"),
        "{err}"
    );
    assert!(
        !dir.join("package.vlt").exists(),
        "nothing written on conflict"
    );

    let out = s.ok("My Tool", &["init", "--template", "cli", "--force"]);
    assert!(out.contains("package `my-tool`"), "{out}");
    let manifest = std::fs::read_to_string(dir.join("package.vlt")).unwrap();
    assert!(manifest.contains("name: \"my-tool\""), "{manifest}");
    assert!(std::fs::read_to_string(dir.join("src/main.vlt"))
        .unwrap()
        .contains("execute(args())"));
    // --force overwrites sources; the README is kept and the .gitignore gets `target/` appended.
    assert_eq!(
        std::fs::read_to_string(dir.join(".gitignore")).unwrap(),
        "*.log\ntarget/\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("README.md")).unwrap(),
        "my readme\n"
    );
    s.ok("My Tool", &["test"]);

    // A second init without --force: package.vlt and the sources conflict.
    assert!(s.fail("My Tool", &["init"]).contains("package.vlt"));
}

#[test]
fn init_keeps_readme_and_takes_a_name() {
    let s = sandbox();
    std::fs::create_dir_all(s.path("w")).unwrap();
    std::fs::write(s.path("w/README.md"), "mine\n").unwrap();
    let out = s.ok("w", &["init", "--name", "textkit", "--template", "lib"]);
    assert!(out.contains("kept existing README.md"), "{out}");
    assert_eq!(
        std::fs::read_to_string(s.path("w/README.md")).unwrap(),
        "mine\n"
    );
    assert!(s.path("w/src/lib.vlt").is_file());
    // Inside that package, init refuses to nest one.
    std::fs::create_dir_all(s.path("w/sub")).unwrap();
    assert!(s
        .fail("w/sub", &["init"])
        .contains("already inside package"));
}

#[test]
fn clean_removes_target_and_reports_bytes() {
    let s = sandbox();
    s.ok("", &["new", "app"]);
    assert!(s.ok("app", &["clean"]).contains("nothing to remove"));
    s.ok("app", &["build"]);
    let out = s.ok("app/src", &["clean"]);
    assert!(out.contains("Removed") && out.contains("iB)"), "{out}");
    assert!(!s.path("app/target").exists());
    assert!(s.fail("", &["clean"]).contains("no `package.vlt`"));
}

#[test]
fn common_mistakes_get_clear_errors() {
    let s = sandbox();
    std::fs::write(s.path("hello.vlt"), "function main() {}\n").unwrap();
    let err = s.fail("", &["run"]);
    assert!(err.contains("no `package.vlt`"), "{err}");
    assert!(
        err.contains("`velt run hello.vlt`") && err.contains("velt init"),
        "{err}"
    );
    let err = s.fail("", &["run", "hello"]);
    assert!(
        err.contains("did you mean") && err.contains("hello.vlt"),
        "{err}"
    );
    let err = s.fail("", &["biuld"]);
    assert!(err.contains("did you mean `velt build`?"), "{err}");
    let err = s.fail("", &["run", "--relase", "hello.vlt"]);
    assert!(err.contains("did you mean `--release`?"), "{err}");
    assert!(err.contains("velt run --help"), "{err}");
}

#[test]
fn help_and_completions() {
    let s = sandbox();
    let overview = s.ok("", &["--help"]);
    for cmd in ["new", "init", "clean", "completions", "websocket"] {
        assert!(overview.contains(cmd), "{cmd}: {overview}");
    }
    let help = s.ok("", &["new", "--help"]);
    assert!(
        help.contains("examples:") && help.contains("--template"),
        "{help}"
    );
    assert_eq!(s.ok("", &["help", "clean"]), s.ok("", &["clean", "-h"]));
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let script = s.ok("", &["completions", shell]);
        assert!(
            script.contains("websocket") && script.contains("--release"),
            "{shell}"
        );
    }
    assert!(s
        .fail("", &["completions", "tcsh"])
        .contains("unknown shell"));
}

#[test]
fn colors_follow_the_environment() {
    let s = sandbox();
    let forced = s
        .command("", &["frobnicate"])
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap();
    assert!(
        text(&forced).contains("\x1b[1;31merror:\x1b[0m"),
        "{}",
        text(&forced)
    );
    let no_color = s
        .command("", &["frobnicate"])
        .env("CLICOLOR_FORCE", "1")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(!text(&no_color).contains('\x1b'));
    // Not a terminal: no colors.
    assert!(!s.fail("", &["frobnicate"]).contains('\x1b'));
}
