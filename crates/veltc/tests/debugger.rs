//! Debugger support: the VS Code templates in `editors/vscode/templates` are consistent (every
//! `preLaunchTask` exists, programs match the builds the tasks make), and with clang and LLDB
//! installed a `velt build --backend llvm` executable resolves breakpoints on `.vlt` lines
//! (checked without running the program, so no debugger permission is needed).

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
fn launch_configs_use_existing_tasks() {
    let tasks = template("tasks.json");
    let labels: Vec<&str> = tasks["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .map(|t| t["label"].as_str().expect("label"))
        .collect();
    let launch = template("launch.json");
    let configs = launch["configurations"].as_array().expect("configurations");
    for kind in ["lldb", "cppvsdbg"] {
        assert!(
            configs.iter().any(|c| c["type"] == kind),
            "no {kind} config"
        );
    }
    for c in configs {
        if let Some(task) = c["preLaunchTask"].as_str() {
            assert!(labels.contains(&task), "unknown task `{task}`");
            let program = c["program"].as_str().expect("program");
            assert!(
                program.starts_with("${workspaceFolder}/target/velt/"),
                "{program}"
            );
        }
    }
    // Line-level debugging needs the LLVM backend (Cranelift emits symbols only).
    for t in tasks["tasks"].as_array().unwrap() {
        let args: Vec<&str> = t["args"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if args.first() == Some(&"build") {
            assert!(
                args.windows(2).any(|w| w == ["--backend", "llvm"]),
                "{args:?}"
            );
        }
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

#[test]
fn lldb_resolves_breakpoints_on_velt_lines() {
    if cfg!(windows) || !velt_codegen_llvm::available() {
        eprintln!("note: needs clang and LLDB on macOS/Linux; skipping");
        return;
    }
    let lldb_runs = crate::no_window::command("lldb")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !lldb_runs {
        eprintln!("note: lldb not installed; skipping");
        return;
    }
    let root = root();
    runtime_support::build_native_runtime(&root);
    let work = work_dir::work_dir(&root, "golden-work-debugger");
    std::fs::create_dir_all(&work).expect("work dir");
    let src = work.join("app.vlt");
    let program = "function add(a: i64, b: i64): i64 {\n  const sum = a + b;\n  return sum;\n}\n\nfunction main() {\n  console.log(add(2, 3));\n}\n";
    std::fs::write(&src, program).expect("write source");
    let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "app.vlt", "--backend", "llvm"])
        .current_dir(&work)
        .output()
        .expect("velt build");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = output_within(
        crate::no_window::command("lldb")
            .args([
                "-b",
                "-o",
                "breakpoint set -f app.vlt -l 2",
                "-o",
                "breakpoint set -f app.vlt -l 7",
            ])
            .arg(work.join("target/velt/app")),
        Duration::from_secs(60),
    )
    .expect("lldb finished");
    let _ = std::fs::remove_dir_all(&work);
    assert!(out.contains("at app.vlt:2"), "{out}");
    assert!(out.contains("at app.vlt:7"), "{out}");
}
