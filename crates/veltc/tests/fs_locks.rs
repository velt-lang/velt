//! velt/fs's byte-range locks between processes (`lockSync`, std/fs/fd/lock.velt; the
//! in-process cases are std/fs/fd/lock.test.velt): a holder in one process keeps another
//! process off its range, a killed holder's locks are given back, and a lock taken with the C
//! library's process-held `fcntl(F_SETLK)` at SQLite's byte offsets conflicts with a Velt
//! holder's, both ways.
#![cfg(unix)]

mod no_window;
mod test_dir;

use std::io::{BufRead, BufReader};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::{Child, Stdio};

/// `hold <path> <offset> <length>` takes an exclusive lock, says `locked` (or `refused`) and
/// stays until it is killed; `try <path> <mode> <offset> <length>` says whether it got the lock;
/// `leave <path> <offset> <length>` starts waiting for the lock and calls `process.exit(7)`.
const PROGRAM: &str = "
import * as fs from \"velt/fs\";

const [what, path] = [process.argv[2], process.argv[3]];
const fd = fs.openSync(path, \"r+\");
if (what == \"hold\") {
  const got = fs.lockSync(fd, { offset: Number(process.argv[4]), length: Number(process.argv[5]) });
  console.log(got ? \"locked\" : \"refused\");
  setInterval(async () => {}, 1000);
} else if (what == \"leave\") {
  // Wants the lock (not awaited), then ends the program itself.
  fs.lock(fd, { offset: Number(process.argv[4]), length: Number(process.argv[5]) });
  console.log(\"leaving\");
  process.exit(7);
} else {
  const mode = process.argv[4] == \"shared\" ? \"shared\" : \"exclusive\";
  const got = fs.lockSync(fd, {
    mode,
    offset: Number(process.argv[5]),
    length: Number(process.argv[6]),
  });
  console.log(got);
  fs.closeSync(fd);
}
";

/// SQLite's PENDING byte: the first of its lock bytes, at 1 GiB.
const PENDING: i64 = 0x4000_0000;

fn build(dir: &Path) -> std::path::PathBuf {
    std::fs::write(dir.join("main.velt"), PROGRAM).unwrap();
    let exe = dir.join("locks");
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

/// Starts a holder of `length` bytes at `offset` and waits for it to say what it got.
fn hold(exe: &Path, file: &Path, offset: i64, length: i64) -> (Child, String) {
    let mut child = std::process::Command::new(exe)
        .arg("hold")
        .arg(file)
        .args([offset.to_string(), length.to_string()])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    (child, line.trim().to_string())
}

/// Whether another process gets a `mode` lock of `length` bytes at `offset`.
fn try_lock(exe: &Path, file: &Path, mode: &str, offset: i64, length: i64) -> String {
    let out = std::process::Command::new(exe)
        .arg("try")
        .arg(file)
        .arg(mode)
        .args([offset.to_string(), length.to_string()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The lock types as numbers: `c_int` on Linux, `c_short` on macOS.
const WRITE: i64 = libc::F_WRLCK as i64;
const READ: i64 = libc::F_RDLCK as i64;
const UNLOCK: i64 = libc::F_UNLCK as i64;

/// The C library's process-held lock (`F_SETLK`) of type `kind` on one byte at `offset`,
/// without waiting: whether it was taken (or, for `UNLOCK`, given back).
fn c_lock(file: &std::fs::File, kind: i64, offset: i64) -> bool {
    // SAFETY: a zeroed `flock` with its fields set, read by `fcntl` for the call only.
    unsafe {
        let mut lock: libc::flock = std::mem::zeroed();
        lock.l_type = kind as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_start = offset;
        lock.l_len = 1;
        libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) == 0
    }
}

#[test]
fn a_holder_keeps_other_processes_off_its_range_until_it_is_gone() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path());
    let file = dir.path().join("data");
    std::fs::write(&file, b"0123456789abcdef").unwrap();

    let (mut holder, said) = hold(&exe, &file, 0, 4);
    assert_eq!(said, "locked");
    assert_eq!(try_lock(&exe, &file, "exclusive", 0, 4), "false");
    assert_eq!(try_lock(&exe, &file, "shared", 3, 1), "false");
    assert_eq!(try_lock(&exe, &file, "exclusive", 4, 4), "true");
    // A second holder of the same range is refused.
    let (mut second, said) = hold(&exe, &file, 2, 4);
    assert_eq!(said, "refused");
    second.kill().unwrap();
    second.wait().unwrap();

    // Killed, not closed: the system gives its locks back.
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(try_lock(&exe, &file, "exclusive", 0, 4), "true");
}

#[test]
fn a_c_process_lock_and_a_velt_lock_conflict_at_sqlites_bytes() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path());
    let path = dir.path().join("db");
    std::fs::write(&path, b"SQLite format 3\0").unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();

    // This process holds the byte with `fcntl(F_SETLK)`: a Velt process is refused.
    assert!(c_lock(&file, WRITE, PENDING));
    assert_eq!(try_lock(&exe, &path, "exclusive", PENDING, 1), "false");
    assert_eq!(try_lock(&exe, &path, "shared", PENDING, 1), "false");
    assert_eq!(try_lock(&exe, &path, "exclusive", PENDING + 1, 1), "true");
    assert!(c_lock(&file, UNLOCK, PENDING));
    assert_eq!(try_lock(&exe, &path, "exclusive", PENDING, 1), "true");

    // A Velt process holds it: `fcntl(F_SETLK)` here is refused, until the holder is gone.
    let (mut holder, said) = hold(&exe, &path, PENDING, 1);
    assert_eq!(said, "locked");
    assert!(!c_lock(&file, WRITE, PENDING));
    assert!(!c_lock(&file, READ, PENDING));
    assert!(c_lock(&file, WRITE, PENDING + 1));
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(c_lock(&file, WRITE, PENDING));
}

/// A lock that is being waited for keeps a program running, as a pending read does, but
/// `process.exit()` ends it at once: nothing blocks in the wait.
#[test]
fn process_exit_ends_a_program_that_waits_for_a_lock() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path());
    let file = dir.path().join("data");
    std::fs::write(&file, b"0123456789abcdef").unwrap();
    let (mut holder, said) = hold(&exe, &file, 0, 4);
    assert_eq!(said, "locked");

    let out = std::process::Command::new(&exe)
        .arg("leave")
        .arg(&file)
        .args(["0", "4"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "leaving");
    assert_eq!(out.status.code(), Some(7));
    // The holder still has it: the leaver took nothing with it.
    assert_eq!(try_lock(&exe, &file, "exclusive", 0, 4), "false");
    holder.kill().unwrap();
    holder.wait().unwrap();
}
