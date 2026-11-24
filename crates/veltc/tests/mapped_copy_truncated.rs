//! `MappedFile.copy` from a file that was cut short while mapped ends the program as reading the
//! cut bytes any other way does (`getUint8`, `bytes`): the same signal and the same output. A
//! page past the file's new end is not there to read, so this is the system's bus error, not an
//! exception (std/fs/mapped_file.velt's class doc).
#![cfg(unix)]

mod no_window;
mod test_dir;

use std::os::unix::process::ExitStatusExt;
use std::path::Path;

/// Maps a 64 KiB file, cuts it to nothing, then reads byte 40,000 of the mapping with
/// `getUint8` (argument `get`) or copies 16 bytes from there (`copy`).
const PROGRAM: &str = "
import * as fs from \"velt/fs\";

const [how, path] = [process.argv[2], process.argv[3]];
fs.writeFileSync(path, \"x\".repeat(65536));
const m = fs.MappedFile.open(path);
const fd = fs.openSync(path, \"r+\");
fs.ftruncateSync(fd, 0);
if (how == \"get\") {
  console.log(m.getUint8(40000));
} else {
  const into = Buffer.alloc(16);
  console.log(m.copy(into, 0, 40000, 40016));
}
console.log(\"not reached\");
";

fn build(dir: &Path) -> std::path::PathBuf {
    std::fs::write(dir.join("main.velt"), PROGRAM).unwrap();
    let exe = dir.join("cut");
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "-o"])
        .arg(&exe)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    exe
}

#[test]
fn a_copy_from_cut_bytes_ends_the_program_as_a_getter_does() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path());
    let run = |how: &str| {
        let file = dir.path().join(format!("data-{how}"));
        crate::no_window::command(&exe)
            .arg(how)
            .arg(&file)
            .output()
            .unwrap()
    };
    let get = run("get");
    let copy = run("copy");
    for out in [&get, &copy] {
        // Output still buffered when the signal comes is lost (the class doc says so), so only
        // the line after the read is looked for.
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("not reached"),
            "the read did not stop the program: {stdout}"
        );
        assert_eq!(
            out.status.signal(),
            Some(libc::SIGBUS),
            "status {:?}, stderr {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(get.stderr, copy.stderr, "the two reads report differently");
}
