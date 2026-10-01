//! The fast paths through the `velt` binary: `velt check` (front end only, text and JSON
//! diagnostics, exit codes) and the link step of debug builds (shared runtime when it is built,
//! static with `VELT_RT_LINK=static`, skipped when nothing changed).

use std::path::Path;
use std::process::{Command, Output};

fn velt(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_velt"));
    cmd.args(args)
        .current_dir(cwd)
        .env_remove("VELT_RT_LIB")
        .env_remove("VELT_RT_LINK");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn check_reports_diagnostics_without_building() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("good.vlt"),
        "function main() {\n  console.log(1);\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("bad.vlt"),
        "function main() {\n  const x: number = \"a\";\n}\n",
    )
    .unwrap();

    let o = velt(dir, &["check", "good.vlt"], &[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(
        !dir.join("target").exists(),
        "check must not build anything"
    );

    let o = velt(dir, &["check", "bad.vlt"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("bad.vlt:2:"),
        "{}",
        text(&o.stderr)
    );

    let o = velt(dir, &["check", "bad.vlt", "--json"], &[]);
    assert_eq!(o.status.code(), Some(1));
    let out = text(&o.stdout);
    assert!(out.contains(r#""errors":1"#), "{out}");
    assert!(
        out.contains(r#""line":2"#) && out.contains(r#""severity":"error""#),
        "{out}"
    );

    let o = velt(dir, &["check", "missing.vlt", "--json"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stdout).contains(r#""location":null"#));
}

/// The `link` stage of `velt build -v --timings`, and whether it was skipped.
fn build(dir: &Path, env: &[(&str, &str)]) -> (bool, String) {
    let o = velt(dir, &["build", "hello.vlt", "--timings"], env);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    (err.contains("up to date"), err)
}

#[test]
fn debug_links_are_skipped_when_nothing_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let hello = "function main() {\n  console.log(\"hi\");\n}\n";
    std::fs::write(dir.join("hello.vlt"), hello).unwrap();
    let exe = dir
        .join("target/velt")
        .join(if cfg!(windows) { "hello.exe" } else { "hello" });

    let (skipped, err) = build(dir, &[]);
    assert!(!skipped, "first build must link:\n{err}");
    let (skipped, err) = build(dir, &[]);
    assert!(skipped, "unchanged rebuild must not link:\n{err}");

    // Another runtime (the static one) is a different link.
    let (skipped, err) = build(dir, &[("VELT_RT_LINK", "static")]);
    assert!(!skipped, "{err}");
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(text(&run.stdout), "hi\n");

    // A changed program links again; a deleted executable too.
    std::fs::write(dir.join("hello.vlt"), hello.replace("hi", "ho")).unwrap();
    let (skipped, err) = build(dir, &[]);
    assert!(!skipped, "{err}");
    std::fs::remove_file(&exe).unwrap();
    let (skipped, err) = build(dir, &[]);
    assert!(!skipped, "{err}");
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(text(&run.stdout), "ho\n");
}

#[test]
fn debug_builds_use_the_shared_runtime_when_built() {
    let built = Path::new(env!("CARGO_BIN_EXE_velt")).parent().unwrap();
    let shared = velt_link::shared_runtime_lib_name(velt_link::TargetOs::host());
    if !built.join(shared).is_file() {
        eprintln!("skipping: {shared} not built (cargo build -p velt_rt_shared)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("hello.vlt"),
        "function main() {\n  console.log(\"shared\");\n}\n",
    )
    .unwrap();
    let (_, err) = build(dir, &[]);
    let entry = if cfg!(windows) {
        "hello.entry.obj"
    } else {
        "hello.entry.o"
    };
    assert!(dir.join("target/velt").join(entry).is_file(), "{err}");
    let exe = dir
        .join("target/velt")
        .join(if cfg!(windows) { "hello.exe" } else { "hello" });
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(text(&run.stdout), "shared\n", "{}", text(&run.stderr));
}
