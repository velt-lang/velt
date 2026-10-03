//! Node's `process.argv` layout, `[runtime, script, ...args]`, for `velt run` (the script is the
//! source file) and for a built program run directly (the script is the executable again, as
//! for a Node single-executable application). Compared with `node` when it is installed.

use std::path::Path;
use std::process::Command;

mod runtime_support;
mod test_dir;

const PROGRAM: &str = r#"function main() {
  const argv = process.argv;
  console.log(argv.length, argv.slice(2));
  console.log(argv[1].endsWith("argv_prog.vlt") || argv[1].endsWith("argv_prog.ts"));
  console.log(argv[1] == argv[0]);
}
"#;

fn velt() -> Command {
    Command::new(env!("CARGO_BIN_EXE_velt"))
}

fn stdout(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

#[test]
fn run_and_built_program_follow_node() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let src = dir.path().join("argv_prog.vlt");
    std::fs::write(&src, PROGRAM).unwrap();
    let args = ["one", "two words"];

    // `velt run`: the script is the source file.
    let run = velt()
        .arg("run")
        .arg(&src)
        .arg("--")
        .args(args)
        .current_dir(dir.path())
        .output()
        .unwrap();
    let ran = stdout(&run);
    assert_eq!(ran, "4 [ 'one', 'two words' ]\ntrue\nfalse\n");

    // A built program: the script is the executable itself.
    let exe = dir.path().join(if cfg!(windows) {
        "argv_prog.exe"
    } else {
        "argv_prog"
    });
    let build = velt()
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .output()
        .unwrap();
    stdout(&build);
    let built = stdout(&Command::new(&exe).args(args).output().unwrap());
    assert_eq!(built, "4 [ 'one', 'two words' ]\nfalse\ntrue\n");
    // The variable `velt run` passes the script in doesn't leak to child processes.
    assert!(std::env::var_os("VELT_SCRIPT").is_none());

    // Node, when installed, prints what `velt run` prints.
    let ts = dir.path().join("argv_prog.ts");
    std::fs::write(&ts, format!("{PROGRAM}main();\n")).unwrap();
    if let Ok(node) = Command::new("node")
        .arg("--experimental-strip-types")
        .arg(&ts)
        .args(args)
        .output()
    {
        if node.status.success() {
            assert_eq!(stdout(&node), ran);
        }
    }
}
