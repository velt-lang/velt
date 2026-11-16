//! Child processes on Windows (std/process/child/child_win32.velt), where they differ from macOS
//! and Linux: batch files are refused however the name is written (Node's CVE-2024-27980 rule;
//! Windows drops trailing dots and spaces from a name, and `cmd.exe` would read an argument as
//! commands), `execSync` finds and checks a program as `exec` does, and running children hold no
//! thread of the executor's blocking pool. The portable behaviour is in the goldens
//! (tests/golden/std/child_*.velt).
#![cfg(windows)]

use std::path::Path;
use std::process::Stdio;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Builds `src` in `dir` and runs it there with `args`: its exit code and output.
fn build_and_run(dir: &Path, name: &str, src: &str, args: &[&str]) -> (Option<i32>, String) {
    runtime_support::build_native_runtime(manifest());
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!("{name}.exe"));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = command(&exe)
        .args(args)
        .current_dir(dir)
        // The lookup looks in the working directory first unless this is set (as some shells
        // and tools set it); the tests expect Windows' default.
        .env_remove("NoDefaultCurrentDirectoryInExePath")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    let err = String::from_utf8_lossy(&o.stderr);
    (o.status.code(), format!("{out}{err}"))
}

#[test]
fn a_batch_file_is_refused_however_its_name_is_written() {
    // Each name opens x.bat or x.cmd (Windows drops a name's trailing dots and spaces), directly,
    // in the working directory, or through `PATH`. Started, the batch file would run the
    // argument's `echo INJECTED` through cmd.exe. Node 22 refuses every one with EINVAL.
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(
        dir.path(),
        "bat",
        r#"import { exec, execSync } from "velt/process/child";
import { writeFileSync } from "velt/fs";
import { cwd } from "velt/process";

async function main(): Promise<i32> {
  writeFileSync("x.bat", "@echo ran %*\r\n");
  writeFileSync("x.cmd", "@echo ran %*\r\n");
  const injecting = "a\"&echo INJECTED&\"";
  const here = cwd();
  const path = new Map<string, string>();
  path.set("PATH", here);
  const names = ["x.bat", "X.BAT", "x.bat.", "x.bat ", "x.bat. .", "x.cmd.", ".\\x.bat.", `${here}\\x.bat.`];
  for (const name of names) {
    let got = "";
    try {
      const r = await exec(name, [injecting]);
      got = `ran ${r.stdout.trim()}`;
    } catch (e) {
      got = e.code;
    }
    let gotSync = "";
    try {
      const r = execSync(name, [injecting]);
      gotSync = `ran ${r.stdout.trim()}`;
    } catch (e) {
      gotSync = e.code;
    }
    console.log(`${JSON.stringify(name)} ${got} ${gotSync}`);
  }
  for (const name of ["x.bat.", "x.cmd "]) {
    try {
      const r = await exec(name, [injecting], { env: path, cwd: "C:\\" });
      console.log(`PATH ${JSON.stringify(name)} ran ${r.stdout.trim()}`);
    } catch (e) {
      console.log(`PATH ${JSON.stringify(name)} ${e.code}`);
    }
  }
  return 0;
}
"#,
        &[],
    );
    assert_eq!(code, Some(0), "{out}");
    let want = r#""x.bat" EINVAL EINVAL
"X.BAT" EINVAL EINVAL
"x.bat." EINVAL EINVAL
"x.bat " EINVAL EINVAL
"x.bat. ." EINVAL EINVAL
"x.cmd." EINVAL EINVAL
".\\x.bat." EINVAL EINVAL
"#;
    assert!(out.starts_with(want), "{out}");
    assert!(!out.contains("INJECTED"), "{out}");
    assert!(
        out.ends_with("PATH \"x.bat.\" EINVAL\nPATH \"x.cmd \" EINVAL\n"),
        "{out}"
    );
}


