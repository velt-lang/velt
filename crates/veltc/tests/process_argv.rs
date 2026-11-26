//! Node's `process.argv` layout, `[runtime, script, ...args]`, for `velt run` (the script is the
//! source file), `velt test` (the test file) and a built program run directly (the script is the
//! executable again, as for a Node single-executable application). The arguments include
//! non-ASCII text, quotes, an empty one and options Velt also has, and the program sits in a
//! directory whose name has a space. Compared with `node` when it is installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

const PROGRAM: &str = r#"function main() {
  const argv = process.argv;
  console.log(argv.length);
  for (const a of argv.slice(2)) {
    console.log(`[${a}]`);
  }
  console.log(argv[1].endsWith("argv_prog.velt") || argv[1].endsWith("argv_prog.ts"));
  console.log(argv[1] == argv[0]);
  console.log(process.env.VELT_SCRIPT == null);
  console.log(argv[0]);
}
"#;

/// velt/process' `argv()` and `args()`, and the two variables velt passes the script and the
/// arguments in (`VELT_SCRIPT`, `VELT_ARGV`), which the program's reads of its environment and
/// its child processes' environment leave out.
const STD_PROGRAM: &str = r#"import { argv, args, envAll } from "velt/process";
import { exec, execSync } from "velt/process/child";
import { platform } from "velt/os";

async function main() {
  const rest = args();
  console.log(argv().length == rest.length + 1);
  for (const a of rest) {
    console.log(`[${a}]`);
  }
  console.log(process.env.VELT_SCRIPT == null);
  console.log(process.env["VELT_ARGV"] == null);
  const keys = Object.keys(envAll());
  console.log(keys.includes("VELT_SCRIPT") || keys.includes("VELT_ARGV"));
  const win = platform() == "win32";
  const env = win ? execSync("cmd", ["/C", "set"]) : execSync("env");
  console.log(env.code);
  console.log(env.stdout.includes("VELT_SCRIPT=") || env.stdout.includes("VELT_ARGV="));
  const later = win ? await exec("cmd", ["/C", "set"]) : await exec("env");
  console.log(later.stdout.includes("VELT_SCRIPT=") || later.stdout.includes("VELT_ARGV="));
}
"#;

/// Arguments that must reach the program unchanged.
const ARGS: &[&str] = &[
    "one",
    "two words",
    "ünïcødé ✓ 日本",
    r#"say "hi""#,
    r"back\slash\",
    "",
    "--release",
    "-h",
];

fn velt() -> Command {
    command(env!("CARGO_BIN_EXE_velt"))
}

fn stdout(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

/// The output expected for [`ARGS`], without the last line (`argv[0]`).
fn expected(script_is_source: bool) -> String {
    let mut out = format!("{}\n", ARGS.len() + 2);
    for a in ARGS {
        out.push_str(&format!("[{a}]\n"));
    }
    out.push_str(&format!(
        "{script_is_source}\n{}\ntrue\n",
        !script_is_source
    ));
    out
}

/// Splits off the last line, `argv[0]`.
fn split_runtime(out: &str) -> (&str, &str) {
    let body = out.trim_end_matches('\n');
    let at = body.rfind('\n').map_or(0, |i| i + 1);
    (&out[..at], &body[at..])
}

/// A directory with a space and a non-ASCII letter in its name.
fn spaced_dir(dir: &Path) -> PathBuf {
    let spaced = dir.join("my dir ü");
    std::fs::create_dir_all(&spaced).unwrap();
    spaced
}

#[test]
fn run_and_built_program_follow_node() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let spaced = spaced_dir(dir.path());
    let src = spaced.join("argv_prog.velt");
    std::fs::write(&src, PROGRAM).unwrap();

    // `velt run`: the script is the source file. What follows the file is the program's, with
    // or without `--`.
    let mut ran = String::new();
    for separator in [&["--"][..], &[]] {
        let run = velt()
            .arg("run")
            .arg(&src)
            .args(separator)
            .args(ARGS)
            .current_dir(&spaced)
            .output()
            .unwrap();
        ran = stdout(&run);
        // `VELT_SCRIPT` reached the program's start-up and was removed: neither the program
        // nor its child processes see it.
        assert_eq!(split_runtime(&ran).0, expected(true), "{separator:?}");
    }

    // A built program: the script is the executable itself, the resolved path it runs from.
    let exe = spaced.join(if cfg!(windows) {
        "argv_prog.exe"
    } else {
        "argv_prog"
    });
    let build = velt()
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .current_dir(&spaced)
        .output()
        .unwrap();
    stdout(&build);
    let built = stdout(&command(&exe).args(ARGS).output().unwrap());
    let (body, runtime) = split_runtime(&built);
    assert_eq!(body, expected(false));
    let resolved = if cfg!(windows) {
        exe.clone()
    } else {
        std::fs::canonicalize(&exe).unwrap()
    };
    assert_eq!(Path::new(runtime), resolved);

    // Node, when installed, prints what `velt run` prints (but for its own path).
    let ts = spaced.join("argv_prog.ts");
    std::fs::write(&ts, format!("{PROGRAM}main();\n")).unwrap();
    if let Ok(node) = command("node")
        .arg("--experimental-strip-types")
        .arg(&ts)
        .args(ARGS)
        .output()
    {
        if node.status.success() {
            assert_eq!(split_runtime(&stdout(&node)).0, split_runtime(&ran).0);
        }
    }
}

/// `velt dev` runs the program inside `velt`, whose own arguments are not the program's: it
/// passes them (and the script) in variables std reads. `process.argv[0]` is the running `velt`.
#[test]
fn dev_passes_the_arguments_and_script() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let spaced = spaced_dir(dir.path());
    let src = spaced.join("argv_prog.velt");
    std::fs::write(&src, PROGRAM).unwrap();
    // `--host`: the program alone, without the supervisor that waits for changes after it.
    let dev = velt()
        .args(["dev", "--host"])
        .arg(&src)
        .arg("--")
        .args(ARGS)
        .current_dir(&spaced)
        .output()
        .unwrap();
    let out = stdout(&dev);
    let (body, runtime) = split_runtime(&out);
    assert_eq!(body, expected(true));
    let velt = Path::new(env!("CARGO_BIN_EXE_velt"));
    let runtime = Path::new(runtime);
    if cfg!(windows) {
        // As Windows names it (GetModuleFileNameW), which may differ in case from Cargo's path.
        assert_eq!(runtime.file_name(), velt.file_name());
    } else {
        assert_eq!(runtime, std::fs::canonicalize(velt).unwrap());
    }
}

/// `argv()` / `args()` of velt/process under `velt run`, `velt dev` and as a built program, and
/// the variables velt passes them in hidden from the program and its children.
#[test]
fn std_arguments_and_hidden_variables() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let spaced = spaced_dir(dir.path());
    let src = spaced.join("std_args.velt");
    std::fs::write(&src, STD_PROGRAM).unwrap();
    let mut want = String::from("true\n");
    for a in ARGS {
        want.push_str(&format!("[{a}]\n"));
    }
    want.push_str("true\ntrue\nfalse\n0\nfalse\nfalse\n");
    for mode in [&["run"][..], &["dev", "--host"]] {
        let out = velt()
            .args(mode)
            .arg(&src)
            .arg("--")
            .args(ARGS)
            .current_dir(&spaced)
            .output()
            .unwrap();
        assert_eq!(stdout(&out), want, "velt {mode:?}");
    }
    let exe = spaced.join(if cfg!(windows) {
        "std_args.exe"
    } else {
        "std_args"
    });
    let build = velt()
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .current_dir(&spaced)
        .output()
        .unwrap();
    stdout(&build);
    let built = command(&exe).args(ARGS).output().unwrap();
    assert_eq!(stdout(&built), want, "the built program");
    // Variables that aren't velt's format leave the OS's arguments in place, still hidden.
    let built = command(&exe)
        .args(ARGS)
        .env("VELT_ARGV", "not 1:a list")
        .env("VELT_SCRIPT", "elsewhere.velt")
        .output()
        .unwrap();
    assert_eq!(stdout(&built), want, "the built program, odd variables");
}

/// A program whose executable is removed while it runs: Linux's `/proc/self/exe` then ends in
/// " (deleted)", which `process.argv[0]` leaves out (the path the program was started as).
#[cfg(target_os = "linux")]
#[test]
fn removed_executable_keeps_its_path() {
    use std::io::Write;
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let src = dir.path().join("gone.velt");
    std::fs::write(
        &src,
        "import { readLineSync } from \"velt/process\";\n\
         function main() {\n  readLineSync();\n  console.log(process.argv[0]);\n}\n",
    )
    .unwrap();
    let exe = dir.path().join("gone");
    stdout(
        &velt()
            .arg("build")
            .arg(&src)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap(),
    );
    let resolved = std::fs::canonicalize(&exe).unwrap();
    let mut child = command(&exe)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::fs::remove_file(&exe).unwrap();
    child.stdin.take().unwrap().write_all(b"\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(stdout(&out).trim_end(), resolved.to_str().unwrap());
}

/// `velt test`: `process.argv[1]` is the test file, as with `node --test`.
#[test]
fn test_files_see_themselves_as_the_script() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let spaced = spaced_dir(dir.path());
    let file = spaced.join("argv.test.velt");
    std::fs::write(
        &file,
        "export function test_script() {\n  const argv = process.argv;\n  \
         assertEq(argv.length, 2);\n  assert(argv[1].endsWith(\"argv.test.velt\"));\n  \
         assert(argv[1] != argv[0]);\n}\n",
    )
    .unwrap();
    let out = velt()
        .arg("test")
        .arg(&spaced)
        .current_dir(&spaced)
        .output()
        .unwrap();
    let text = stdout(&out);
    assert!(text.contains("test_script"), "{text}");
}

/// A prompt written before a blocking read of stdin is out before the program waits, as Node
/// flushes stdout first: the caller reading it can answer. Without the flush both would wait.
#[test]
fn stdout_is_flushed_before_stdin_is_read() {
    use std::io::{Read, Write};
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("prompt.velt");
    std::fs::write(
        &file,
        "import { readLineSync } from \"velt/process\";\n\
         function main() {\n  console.log(\"name?\");\n  const n = readLineSync();\n  \
         console.log(`hi ${n}`);\n}\n",
    )
    .unwrap();
    let exe = dir.path().join("prompt");
    let built = velt()
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    stdout(&built);
    let mut child = command(&exe)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut got = Vec::new();
        let mut byte = [0u8; 1];
        while out.read(&mut byte).unwrap_or(0) == 1 {
            got.push(byte[0]);
            if got.ends_with(b"name?\n") {
                let _ = tx.send(());
            }
        }
    });
    let prompted = rx.recv_timeout(std::time::Duration::from_secs(60)).is_ok();
    let mut input = child.stdin.take().unwrap();
    let _ = input.write_all(b"ada\n");
    drop(input);
    let _ = child.wait();
    assert!(
        prompted,
        "the prompt did not arrive before the program read stdin"
    );
}
