//! Standard output of a program whose tasks run on several workers: what a task prints before
//! the workers go idle reaches a pipe while the program still runs, and lines printed by many
//! tasks at once arrive whole, in order per task, in large blocks.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

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
    let exe = dir.join(name);
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

/// Like a server that logs and then waits for connections, for a day.
const IDLE: &str = r#"async function main() {
  console.log("ready");
  await sleep(86_400_000);
}
"#;

/// Kills the child when dropped, so a failing assertion does not leave it sleeping.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The program sleeps for a day after printing, so the line can only arrive through the flush
/// when the workers go idle: no deadline on how soon (a loaded machine may take seconds just to
/// start the process), only a hang guard in case it never does.
#[test]
fn piped_output_is_flushed_when_workers_idle() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "idle", IDLE);
    let mut child = KillOnDrop(command(&exe).stdout(Stdio::piped()).spawn().unwrap());
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = std::io::BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(read.map(|_| line));
    });
    let line = rx
        .recv_timeout(Duration::from_secs(120))
        .expect("no output after 120 s: buffered output is not flushed when the workers idle")
        .unwrap();
    assert_eq!(line.trim_end(), "ready");
    // The line arrived while the program sleeps: it was flushed at idle, not at exit.
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "the program exited before its output arrived"
    );
    drop(child);
}

/// 64 tasks each print 1000 lines of several arguments. Even tasks yield every 100 lines
/// (moving between workers), odd ones print everything in one go (crossing the buffer size
/// mid-line).
const STRESS: &str = r#"async function printer(id: i64): Promise<void> {
  const pad = "x".repeat(120);
  for (let line = 0; line < 1000; line++) {
    const n = (id * 7 + line * 13) % 120;
    console.log(`t=${id}`, `l=${line}`, pad.slice(0, n), n);
    if ((line + 1) % ((id & 1) == 1 ? 1000 : 100) == 0) {
      await yieldNow();
    }
  }
}

async function main() {
  const tasks: Promise<void>[] = [];
  for (let i = 0; i < 64; i++) {
    tasks.push(spawn(printer(i)));
  }
  for (const t of tasks) {
    await t;
  }
}
"#;

/// Check one stress line: `t=<id> l=<line> <n x's> <n>`; returns (id, line).
fn parse_stress_line(line: &str) -> (usize, usize) {
    let num = |s: Option<&str>| -> usize {
        s.and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("corrupted line: {line:?}"))
    };
    let parts: Vec<&str> = line.split(' ').collect();
    let [t, l, pad, n] = parts[..] else {
        panic!("corrupted line: {line:?}")
    };
    let (id, no, n) = (
        num(t.strip_prefix("t=")),
        num(l.strip_prefix("l=")),
        num(Some(n)),
    );
    let pad_ok = pad.len() == n && pad.bytes().all(|b| b == b'x');
    assert!(
        pad_ok && n == (id * 7 + no * 13) % 120,
        "corrupted line: {line:?}"
    );
    (id, no)
}

/// The `writes=` count of the `stdout stats:` line a debug runtime prints at exit.
fn stdout_writes(stderr: &str) -> usize {
    stderr
        .lines()
        .find_map(|l| l.strip_prefix("stdout stats: writes="))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or_else(|| panic!("no `stdout stats` line in stderr:\n{stderr}"))
}

/// Lines from 64 tasks on 8 workers arrive whole and in order per task, and piped output goes out
/// in large blocks: the debug runtime counts its writes to stdout (`VELT_STDOUT_STATS=1`), which
/// must be far fewer than the lines (one write per line is the regression this guards against). A
/// count, not a duration: a small write to a pipe costs 25 times more on macOS than on Linux, so a
/// time limit set for one platform misses the regression on the other.
#[test]
fn concurrent_tasks_never_interleave_within_a_line() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "stdout_stress", STRESS);
    for _ in 0..3 {
        let out = command(&exe)
            .env("VELT_THREADS", "8")
            .env("VELT_STDOUT_STATS", "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
        assert!(out.status.success(), "{}\n{stderr}", out.status);
        let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        let mut next = vec![0usize; 64];
        let mut count = 0;
        for line in stdout.lines() {
            let (id, no) = parse_stress_line(line);
            assert_eq!(no, next[id], "task {id} lines out of order");
            next[id] += 1;
            count += 1;
        }
        assert_eq!(count, 64_000);
        // The counter exists in the debug runtime, which a debug build of the program links.
        if cfg!(debug_assertions) {
            let writes = stdout_writes(&stderr);
            assert!(
                writes <= count / 100,
                "stdout stress: {writes} writes for {count} lines (piped output must be block-buffered)"
            );
        }
    }
}
