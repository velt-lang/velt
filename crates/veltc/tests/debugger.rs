//! Debugger support: the VS Code templates in `editors/vscode/templates` use the extension's
//! `velt` debug type and existing tasks, and with LLDB installed an executable of the default
//! (Cranelift) build, and with clang one of `--backend llvm`, resolves breakpoints on `.vlt` lines
//! in the right functions (checked without running the program, so no debugger permission is
//! needed). On macOS the debug info stays in the object, which the executable's debug map names.
//! Where LLDB may run programs, a default build also shows local variables with their source
//! types (skipped where it may not).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

mod no_window;
mod runtime_support;
mod work_dir;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn template(name: &str) -> Value {
    let path = root().join("editors/vscode/templates").join(name);
    let text = std::fs::read_to_string(&path).expect("template");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn launch_configs_use_the_velt_debug_type() {
    let tasks = template("tasks.json");
    let labels: Vec<&str> = tasks["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .map(|t| t["label"].as_str().expect("label"))
        .collect();
    let launch = template("launch.json");
    let configs = launch["configurations"].as_array().expect("configurations");
    for request in ["launch", "attach"] {
        assert!(
            configs.iter().any(|c| c["request"] == request),
            "no {request} config"
        );
    }
    for c in configs {
        // The extension builds and picks the debugger; nothing names a build or a debugger.
        assert_eq!(c["type"], "velt", "{c}");
        if let Some(task) = c["preLaunchTask"].as_str() {
            assert!(labels.contains(&task), "unknown task `{task}`");
        }
    }
    // No build needs clang: debugging uses the default (Cranelift) build.
    for t in tasks["tasks"].as_array().unwrap() {
        let args: Vec<&str> = t["args"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(!args.contains(&"--backend"), "{args:?}");
    }
}

/// Run `cmd`, killing it after `limit`; its combined output.
fn output_within(cmd: &mut Command, limit: Duration) -> Option<String> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

const PROGRAM: &str = "function add(a: i64, b: i64): i64 {\n  const sum = a + b;\n  return sum;\n}\n\nfunction main() {\n  console.log(add(2, 3));\n}\n";

fn lldb_runs() -> bool {
    let runs = crate::no_window::command("lldb")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !runs {
        eprintln!("note: lldb not installed; skipping");
    }
    runs
}

/// Build `source` as `app.vlt` with `args` in a fresh work dir; the dir and the executable.
fn build(work_name: &str, source: &str, args: &[&str]) -> (PathBuf, PathBuf) {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let work = work_dir::work_dir(&root, work_name);
    std::fs::create_dir_all(&work).expect("work dir");
    std::fs::write(work.join("app.vlt"), source).expect("write source");
    let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "app.vlt"])
        .args(args)
        .current_dir(&work)
        .output()
        .expect("velt build");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let exe = work.join("target/velt/app");
    (work, exe)
}

/// LLDB in batch mode running `commands` on `exe`; its output.
fn lldb(exe: &Path, commands: &[&str]) -> String {
    let mut cmd = crate::no_window::command("lldb");
    cmd.arg("-b");
    for c in commands {
        cmd.args(["-o", c]);
    }
    output_within(cmd.arg(exe), Duration::from_secs(60)).expect("lldb finished")
}

/// Build `PROGRAM` as `app.vlt` with `args`, then set breakpoints on lines 2 (in `add`) and 7
/// (in `main`) with LLDB; its output.
fn breakpoints(work_name: &str, args: &[&str]) -> String {
    let (work, exe) = build(work_name, PROGRAM, args);
    if cfg!(target_os = "macos") {
        // The debug info stays in the object; the debug map must name it by its absolute path.
        let map = crate::no_window::command("nm")
            .args(["-ap"])
            .arg(&exe)
            .output()
            .expect("nm");
        let map = String::from_utf8_lossy(&map.stdout);
        let object = work.join("target/velt/app.o");
        assert!(
            map.lines().any(|l| l.contains(" OSO ")
                && l.split(" OSO ").nth(1).is_some_and(|p| {
                    Path::new(p.trim()).canonicalize().ok() == object.canonicalize().ok()
                })),
            "no debug map entry for {}:\n{map}",
            object.display()
        );
    }
    let out = lldb(
        &exe,
        &[
            "breakpoint set -f app.vlt -l 2",
            "breakpoint set -f app.vlt -l 7",
        ],
    );
    let _ = std::fs::remove_dir_all(&work);
    out
}

const LOCALS: &str = r#"class Point {
  x: number;
  y: number;
  constructor(x: number, y: number) {
    this.x = x;
    this.y = y;
  }
}

enum Color {
  Red,
  Green = 5,
}

function area(w: number, h: number): number {
  const a = w * h;
  return a;
}

function main() {
  const xs: number[] = [1, 2, 3];
  const p = new Point(1, 2);
  const maybe: Point | null = null;
  const c = Color.Green;
  let count: i64 = 0;
  for (const x of xs) {
    count += 1;
  }
  console.log(xs.length, p.x, maybe == null, c, count, area(2, 3));
}
"#;

/// Variables with their source types, values and fields, in the default build (the program
/// runs under LLDB, which needs permission to debug on some systems).
#[test]
fn lldb_shows_locals_in_the_default_build() {
    if cfg!(windows) {
        eprintln!("note: Windows builds carry no DWARF yet; skipping");
        return;
    }
    if !lldb_runs() {
        return;
    }
    let (work, exe) = build("golden-work-debugger-locals", LOCALS, &[]);
    let out = lldb(
        &exe,
        &[
            "breakpoint set -f app.vlt -l 29",
            "breakpoint set -f app.vlt -l 17",
            "run",
            "frame variable",
            "frame variable -P 1 p",
            "frame variable xs.data[2]",
            "continue",
            "frame variable",
            "kill",
        ],
    );
    let _ = std::fs::remove_dir_all(&work);
    if !out.contains("stop reason = breakpoint") {
        eprintln!("note: LLDB could not run the program here; skipping\n{out}");
        return;
    }
    for expected in [
        "(number[]) xs = {",
        "len = 3",
        "(i64) count = 3",
        "(Color) c = Green",
        "(Point | null) maybe = NULL",
        "y = 2",
        "(number) xs.data[2] = 3",
        "(number) w = 2",
        "(number) h = 3",
        "(number) a = 6",
    ] {
        assert!(out.contains(expected), "no `{expected}` in:\n{out}");
    }
}

/// What F5 in VS Code debugs: the default build, which needs no clang.
#[test]
fn lldb_resolves_breakpoints_in_the_default_build() {
    if cfg!(windows) {
        eprintln!("note: Windows builds carry no DWARF yet; skipping");
        return;
    }
    if !lldb_runs() {
        return;
    }
    let out = breakpoints("golden-work-debugger-cl", &[]);
    // Each in its own function (regression: on macOS line 2 resolved to `add` at line 7, the
    // rows of `main`).
    let first = out.lines().find(|l| l.starts_with("Breakpoint 1:"));
    assert!(
        first.is_some_and(|l| l.contains("add") && l.contains("at app.vlt:2")),
        "{out}"
    );
    assert!(out.contains("at app.vlt:7"), "{out}");
}

#[test]
fn lldb_resolves_breakpoints_in_llvm_builds() {
    if cfg!(windows) || !velt_codegen_llvm::available() {
        eprintln!("note: needs clang and LLDB on macOS/Linux; skipping");
        return;
    }
    if !lldb_runs() {
        return;
    }
    let out = breakpoints("golden-work-debugger", &["--backend", "llvm"]);
    assert!(out.contains("at app.vlt:2"), "{out}");
    assert!(out.contains("at app.vlt:7"), "{out}");
}
