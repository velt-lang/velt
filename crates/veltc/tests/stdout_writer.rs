//! std's stdout and stderr writer (std/lang/hooks/stdout*.velt, #2196), as Node behaves: every
//! exit a program takes writes out everything it printed (the end of `main`, `process.exit`, an
//! uncaught error), stdout and stderr sent to one pipe interleave in the order the program wrote
//! them, large output reaches a pipe complete, and a non-blocking pipe that is full makes the
//! writer wait instead of losing bytes.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn velt() -> Command {
    command(env!("CARGO_BIN_EXE_velt"))
}

/// Build `source` as `name` in `dir`.
fn build(dir: &Path, name: &str, source: &str) -> PathBuf {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, source).unwrap();
    let exe = dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    let built = velt()
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    exe
}

/// Prints many lines from tasks and a partial line, then ends as its argument says: `exit`
/// (`process.exit(3)`), `throw` (an uncaught error) or nothing (the end of `main`).
const EXITS: &str = r#"async function printer(id: i64): Promise<void> {
  for (let i = 0; i < 2000; i++) {
    console.log(`task ${id} line ${i}`);
  }
}

async function main() {
  const tasks: Promise<void>[] = [];
  for (let id = 0; id < 4; id++) {
    tasks.push(printer(id));
  }
  for (const t of tasks) {
    await t;
  }
  process.stdout.write("no newline at the end");
  const how = process.argv.length > 2 ? process.argv[2] : "";
  if (how == "exit") {
    process.exit(3);
  }
  if (how == "throw") {
    throw new Error("boom");
  }
}
"#;

fn check_complete(out: &str) {
    for id in 0..4 {
        for i in 0..2000 {
            let line = format!("task {id} line {i}\n");
            assert!(out.contains(&line), "missing {line:?}");
        }
    }
    assert!(
        out.ends_with("no newline at the end"),
        "{:?}",
        &out[out.len().saturating_sub(80)..]
    );
    assert_eq!(out.lines().count(), 8001);
}

#[test]
fn every_exit_writes_out_everything_printed() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "exits", EXITS);
    for (how, code) in [("", 0), ("exit", 3), ("throw", 1)] {
        let o = command(&exe).arg(how).output().unwrap();
        assert_eq!(o.status.code(), Some(code), "{how:?}");
        check_complete(&String::from_utf8_lossy(&o.stdout));
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(err.contains("boom"), how == "throw", "{how:?}: {err}");
    }
}

/// stdout and stderr on one pipe: each stderr write goes out after everything stdout holds.
const ORDER: &str = r#"function main() {
  console.log("one");
  console.error("two");
  process.stdout.write("three ");
  console.log(4);
  process.stderr.write("five\n");
  throw new Error("six");
}
"#;

#[test]
fn stdout_and_stderr_on_one_pipe_keep_the_program_order() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "order", ORDER);
    let o = if cfg!(windows) {
        command("cmd")
            .arg("/C")
            .arg(format!("\"{}\" 2>&1", exe.display()))
            .output()
            .unwrap()
    } else {
        command("sh")
            .arg("-c")
            .arg("\"$0\" 2>&1")
            .arg(&exe)
            .output()
            .unwrap()
    };
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    assert!(out.starts_with("one\ntwo\nthree 4\nfive\n"), "{out:?}");
    assert!(out[18..].contains("six"), "{out:?}");
}

/// About 1.2 MB: lines, then a large byte array (written past the buffers), then two lines.
const BIG: &str = r#"import { stdout } from "velt/process";

function main() {
  const line = "x".repeat(99);
  for (let i = 0; i < 10000; i++) {
    console.log(line);
  }
  stdout.write(new Array<u8>(200000).fill(121));
  console.log("");
  console.log("end");
}
"#;

const BIG_LEN: usize = 10000 * 100 + 200000 + 1 + 4;

fn check_big(out: &[u8]) {
    assert_eq!(out.len(), BIG_LEN);
    let lines = 10000 * 100;
    assert!(out[..lines]
        .chunks(100)
        .all(|l| l[..99].iter().all(|&b| b == b'x') && l[99] == b'\n'));
    assert!(out[lines..lines + 200000].iter().all(|&b| b == b'y'));
    assert_eq!(&out[lines + 200000..], b"\nend\n");
}

#[test]
fn large_output_reaches_a_pipe_complete() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "big", BIG);
    let mut child = command(&exe).stdout(Stdio::piped()).spawn().unwrap();
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    assert!(child.wait().unwrap().success());
    check_big(&out);
}

/// A non-blocking pipe that fills up (`EAGAIN`) while its reader is busy: the writer waits for
/// room, as Node's synchronous writes to a pipe do, and loses nothing.
#[cfg(unix)]
#[test]
fn a_full_non_blocking_pipe_is_waited_for() {
    use std::os::fd::{FromRawFd, OwnedFd};
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "big_nb", BIG);
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors `pipe` makes.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: both descriptors are fresh and owned from here on.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // SAFETY: `fds[1]` is open; only its status flags change.
    unsafe {
        let flags = libc::fcntl(fds[1], libc::F_GETFL);
        assert!(libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK) == 0);
    }
    // A small pipe on Linux (16 KiB, macOS's starting size), so it is full once it holds that.
    #[cfg(target_os = "linux")]
    // SAFETY: `fds[1]` is open; only its capacity changes.
    unsafe {
        assert!(
            libc::fcntl(fds[1], libc::F_SETPIPE_SZ, FULL as libc::c_int) >= FULL as libc::c_int
        );
    }
    let mut child = command(&exe).stdout(Stdio::from(write)).spawn().unwrap();
    // The reader starts only once the pipe is full: the writer has met `EAGAIN` by then.
    let start = std::time::Instant::now();
    while queued(fds[0]) < FULL {
        assert!(start.elapsed().as_secs() < 60, "the pipe never filled");
        std::thread::yield_now();
    }
    let mut out = Vec::new();
    std::fs::File::from(read).read_to_end(&mut out).unwrap();
    assert!(child.wait().unwrap().success());
    check_big(&out);
}

/// How full a pipe must be before the reader of [`a_full_non_blocking_pipe_is_waited_for`] starts.
#[cfg(unix)]
const FULL: usize = 16384;

/// The bytes waiting in the pipe whose read end is `fd`.
#[cfg(unix)]
fn queued(fd: i32) -> usize {
    let mut n: libc::c_int = 0;
    // SAFETY: `fd` is open; `FIONREAD` writes one `int`.
    assert_eq!(unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) }, 0);
    n as usize
}
