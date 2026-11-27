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

/// The arguments are read once per process: a program that sets the variables velt passes them
/// in afterwards still sees its own, as in Node, where `process.env` doesn't feed `process.argv`.
const READ_ONCE_PROGRAM: &str = r#"import { argv, setEnv } from "velt/process";

function main() {
  const node = JSON.stringify(process.argv);
  const all = JSON.stringify(argv());
  setEnv("VELT_SCRIPT", "elsewhere.velt");
  setEnv("VELT_ARGV", "5:other");
  console.log(JSON.stringify(process.argv) == node);
  console.log(JSON.stringify(argv()) == all);
  console.log(JSON.stringify(process.argv.slice(2)));
}
"#;

#[test]
fn arguments_are_read_once() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let src = dir.path().join("read_once.velt");
    std::fs::write(&src, READ_ONCE_PROGRAM).unwrap();
    let want = "true\ntrue\n[\"a b\",\"\",\"ü\"]\n";
    for mode in [&["run"][..], &["dev", "--host"]] {
        let out = velt()
            .args(mode)
            .arg(&src)
            .arg("--")
            .args(["a b", "", "ü"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(stdout(&out), want, "velt {mode:?}");
    }
}

/// The arguments are read before the environment first changes (std/process/env_posix.velt,
/// env_win32.velt and env_wasm.velt `argsReady`): the readers take velt's variables without the
/// environment's lock, so std's `setEnv` and `removeEnv` finish that read first. A program that
/// sets or removes the variables before it reads its arguments still sees its own. And once it
/// has set or removed one, that variable is its own, read back as set, also by a child, as in
/// Node.
const CHANGED_FIRST_PROGRAM: &str = r#"import { argv, envAll, removeEnv, setEnv } from "velt/process";
import { execSync } from "velt/process/child";
import { platform } from "velt/os";

function main() {
  setEnv("VELT_ARGV", "5:other");
  removeEnv("VELT_SCRIPT");
  console.log(process.argv[1].endsWith("changed_first.velt"));
  console.log(JSON.stringify(argv().slice(1)));
  console.log(process.env.VELT_ARGV, process.env.VELT_SCRIPT === undefined);
  setEnv("VELT_SCRIPT", "/fake/script.velt");
  console.log(process.env["VELT_SCRIPT"], Object.keys(envAll()).includes("VELT_SCRIPT"));
  const env = platform() == "win32" ? execSync("cmd", ["/C", "set"]) : execSync("env");
  console.log(env.stdout.includes("VELT_ARGV=5:other"), env.stdout.includes("VELT_SCRIPT=/fake"));
}
"#;

#[test]
fn arguments_are_read_before_the_environment_changes() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let src = dir.path().join("changed_first.velt");
    std::fs::write(&src, CHANGED_FIRST_PROGRAM).unwrap();
    let want = "true\n[\"a b\",\"\",\"ü\"]\n5:other true\n/fake/script.velt true\ntrue true\n";
    for mode in [&["run"][..], &["run", "--release"], &["dev", "--host"]] {
        let out = velt()
            .args(mode)
            .arg(&src)
            .arg("--")
            .args(["a b", "", "ü"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(stdout(&out), want, "velt {mode:?}");
    }
}

/// The arguments' prebuilt object builds for each native target, so a warm build cache serves it
/// on every OS: no reader reaches state a program shares (`velt check --target` fills the cache
/// and then finds the object; on Windows, `passedVar` took its names through string methods,
/// whose position cache is a program's thread-local value, and the object was refused).
#[test]
fn prebuilt_readers_build_for_every_native_target() {
    let dir = test_dir::TestDir::new();
    let src = dir.path().join("targets.velt");
    std::fs::write(
        &src,
        "function main() {\n  console.log(process.argv.length);\n}\n",
    )
    .unwrap();
    for target in [
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
    ] {
        let cache = dir.path().join(format!("cache-{target}"));
        for (fill, want) in [(true, false), (false, true)] {
            let mut cmd = velt();
            cmd.args(["check", "--target", target])
                .arg(&src)
                .env("VELT_BUILD_CACHE", &cache)
                .env("VELT_STD_OBJECT_STATS", "1")
                .env_remove("VELT_STD_OBJECT_FILL")
                .env_remove("VELT_PREBUILT_HOOKS");
            if fill {
                cmd.env("VELT_PREBUILT_HOOKS", "fill");
            }
            let out = cmd.output().unwrap();
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{target}: {err}");
            assert!(
                !err.contains("no object of `lang_hooks_process_argv`"),
                "{target}: {err}"
            );
            let found = err.contains("process_argv: object found");
            assert_eq!(found, want, "{target}, fill {fill}: {err}");
        }
    }
}

/// `process.argv` read at several places compiles its reader once: the reader takes no caller's
/// location (a std function that may panic gets a copy per call site, velt_vir track_caller.rs)
/// and is not inlined into the caller, so a program's code does not grow by a copy of it per
/// read (a program reading it twice compiled 2.3 times as long as with the runtime's extern).
#[test]
fn argv_reader_is_compiled_once() {
    let dir = test_dir::TestDir::new();
    let src = dir.path().join("argv_sites.velt");
    std::fs::write(
        &src,
        "function main() {\n  let n = process.argv.length;\n  n += process.argv[0].length;\n  \
         n += process.argv.slice(2).length;\n  console.log(n > 0);\n}\n",
    )
    .unwrap();
    let out = velt()
        .args(["build", "--release", "--emit", "vir"])
        .arg(&src)
        .output()
        .unwrap();
    let vir = stdout(&out);
    let fns: Vec<&str> = vir.lines().filter(|l| l.starts_with("fn#")).collect();
    let lists: Vec<&&str> = fns.iter().filter(|l| l.contains("N7argList")).collect();
    assert_eq!(lists.len(), 1, "{lists:#?}");
    assert!(!lists[0].contains("argList_L"), "{}", lists[0]);
    // The reading itself (the files, the variables) is one function more, out of line.
    let reads = fns.iter().filter(|l| l.contains("N12readNodeArgv")).count();
    assert_eq!(reads, 1, "{fns:#?}");
}

/// With the build cache holding the arguments' prebuilt object (std/lang/hooks/process_argv.velt,
/// velt_common `std_hooks/prebuilt.rs`), a program calls it instead of compiling the readers in,
/// and reads the same lists: `process.argv`, `argv()` and `args()`, and the hidden variables.
#[test]
fn prebuilt_readers_give_the_same_lists() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let spaced = spaced_dir(dir.path());
    let cache = dir.path().join("cache");
    for (name, src) in [("argv_prog", PROGRAM), ("std_args", STD_PROGRAM)] {
        let file = spaced.join(format!("{name}.velt"));
        std::fs::write(&file, src).unwrap();
        let exe = spaced.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        });
        // The module loaded (no objects), then the object stored, then the object served.
        let mut outputs = vec![];
        for (mode, served) in [(Some("0"), false), (Some("fill"), false), (None, true)] {
            let mut cmd = velt();
            cmd.arg("build")
                .arg(&file)
                .arg("-o")
                .arg(&exe)
                .current_dir(&spaced)
                .env("VELT_BUILD_CACHE", &cache)
                .env("VELT_STD_OBJECT_STATS", "1")
                .env("VELT_BUILD_STAMP", "off")
                .env_remove("VELT_STD_OBJECT_FILL")
                .env_remove("VELT_PREBUILT_HOOKS");
            if let Some(m) = mode {
                cmd.env("VELT_PREBUILT_HOOKS", m);
            }
            let build = cmd.output().unwrap();
            stdout(&build);
            let err = String::from_utf8_lossy(&build.stderr);
            if mode != Some("fill") {
                let found = err.contains("process_argv: object found");
                assert_eq!(found, served, "{name}, {mode:?}: {err}");
            }
            let run = command(&exe).args(ARGS).output().unwrap();
            outputs.push(stdout(&run));
        }
        assert_eq!(outputs[0], outputs[2], "{name}: loaded and prebuilt");
        assert_eq!(outputs[0], outputs[1], "{name}: loaded and prebuilt");
        if name == "argv_prog" {
            let (body, runtime) = split_runtime(&outputs[2]);
            assert_eq!(body, expected(false), "{name}");
            assert!(!runtime.is_empty());
        }
    }
    // `velt check` takes the object too.
    let check = velt()
        .arg("check")
        .arg(spaced.join("argv_prog.velt"))
        .current_dir(&spaced)
        .env("VELT_BUILD_CACHE", &cache)
        .env("VELT_STD_OBJECT_STATS", "1")
        .env_remove("VELT_STD_OBJECT_FILL")
        .env_remove("VELT_PREBUILT_HOOKS")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&check.stderr);
    assert!(check.status.success(), "{err}");
    assert!(err.contains("process_argv: object found"), "{err}");
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
