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
  console.log(argv[1].endsWith("argv_prog.vlt") || argv[1].endsWith("argv_prog.ts"));
  console.log(argv[1] == argv[0]);
  console.log(process.env.VELT_SCRIPT == null);
  console.log(argv[0]);
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
    let src = spaced.join("argv_prog.vlt");
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

/// `velt test`: `process.argv[1]` is the test file, as with `node --test`.
#[test]
fn test_files_see_themselves_as_the_script() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let spaced = spaced_dir(dir.path());
    let file = spaced.join("argv.test.vlt");
    std::fs::write(
        &file,
        "export function test_script() {\n  const argv = process.argv;\n  \
         assertEq(argv.length, 2);\n  assert(argv[1].endsWith(\"argv.test.vlt\"));\n  \
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
