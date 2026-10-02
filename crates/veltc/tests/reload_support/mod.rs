//! Support for tests/reload.rs: a `velt dev` session with captured (timestamped) output, a tiny
//! HTTP client, requests left running in the background, and a probe that counts refused
//! connections.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(windows)]
mod job;

/// How long one expectation may take (an `--exe` rebuild in a debug build of `velt` included).
/// Generous like the golden harness's: under a full gate run the machine is saturated, and a
/// first `--exe` build took over 30 s there.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Build the runtime staticlib `velt --exe` links (as the golden harness does), in this test's
/// profile: `velt` looks for it next to itself.
pub fn build_runtime() {
    // Already built by the gate (see `tests/runtime_support/mod.rs`).
    if cfg!(debug_assertions) && std::env::var_os("VELT_RT_PREBUILT").is_some_and(|v| v == "1") {
        return;
    }
    let profile: &[&str] = if cfg!(debug_assertions) {
        &[]
    } else {
        &["--release"]
    };
    let status = Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", "velt_rt"])
        .args(profile)
        .status()
        .expect("run cargo");
    assert!(status.success(), "cargo build -p velt_rt failed");
}

/// Captured lines of both output streams, with the time each arrived.
#[derive(Default)]
struct Log {
    stdout: Vec<(Instant, String)>,
    stderr: Vec<(Instant, String)>,
}

/// Positions in the output: expectations of a step look only at lines after its mark.
#[derive(Clone, Copy, Default)]
pub struct Mark {
    stdout: usize,
    stderr: usize,
}

/// A running `velt dev` (in its own process group on Unix and a job object on Windows, so the
/// programs it starts end with it).
pub struct Dev {
    child: Child,
    log: Arc<Mutex<Log>>,
    #[cfg(windows)]
    job: job::Job,
}

impl Dev {
    /// `velt dev <mode flags> main.vlt` in `dir`.
    pub fn start(dir: &Path, mode: &[&str]) -> Dev {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_velt"));
        cmd.arg("dev")
            .args(mode)
            .arg("main.vlt")
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn().expect("start velt dev");
        #[cfg(windows)]
        let job = job::Job::holding(&child);
        let log = Arc::new(Mutex::new(Log::default()));
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        capture(out, log.clone(), |l| &mut l.stdout);
        capture(err, log.clone(), |l| &mut l.stderr);
        Dev {
            child,
            log,
            #[cfg(windows)]
            job,
        }
    }

    /// Send `signal` to the supervisor alone (as a process manager would).
    #[cfg(unix)]
    pub fn signal(&self, signal: i32) {
        let pid = i32::try_from(self.child.id()).expect("pid");
        // SAFETY: plain syscall on our own child's pid (not yet reaped, so not reused).
        unsafe { libc::kill(pid, signal) };
    }

    /// The supervisor's child processes (Linux: `/proc/<pid>/task/<pid>/children`).
    #[cfg(target_os = "linux")]
    pub fn children(&self) -> Vec<u32> {
        let pid = self.child.id();
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|p| p.parse().ok())
            .collect()
    }

    /// Wait up to `limit` for the supervisor to exit; its exit code (128 + signal if a signal
    /// ended it).
    #[cfg(unix)]
    pub fn wait_exit(&mut self, limit: Duration) -> Option<i32> {
        use std::os::unix::process::ExitStatusExt;
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status.code().or(status.signal().map(|s| 128 + s));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    /// The current end of both streams.
    pub fn mark(&self) -> Mark {
        let log = self.log.lock().unwrap();
        Mark {
            stdout: log.stdout.len(),
            stderr: log.stderr.len(),
        }
    }

    /// Wait for a stderr line after `mark` containing `needle`; when it arrived.
    pub fn wait_stderr(&self, mark: Mark, needle: &str) -> Result<Instant, String> {
        self.wait_stderr_any(mark, &[needle])
    }

    /// Wait for a stderr line after `mark` containing any of `needles`; when it arrived.
    pub fn wait_stderr_any(&self, mark: Mark, needles: &[&str]) -> Result<Instant, String> {
        let mut found = None;
        self.wait(|log| {
            found = log.stderr[mark.stderr..]
                .iter()
                .find(|(_, l)| needles.iter().any(|n| l.contains(n)))
                .map(|(at, _)| *at);
            found.is_some()
        })
        .map_err(|log| format!("no {needles:?} on stderr:\n{log}"))?;
        Ok(found.unwrap_or_else(Instant::now))
    }

    /// The stderr lines after `mark`.
    #[allow(dead_code)]
    pub fn stderr_since(&self, mark: Mark) -> Vec<String> {
        let log = self.log.lock().unwrap();
        log.stderr[mark.stderr..]
            .iter()
            .map(|(_, l)| l.clone())
            .collect()
    }

    /// Wait for the stdout line `line` after `mark`.
    pub fn wait_stdout(&self, mark: Mark, line: &str) -> Result<(), String> {
        self.wait(|log| log.stdout[mark.stdout..].iter().any(|(_, l)| l == line))
            .map_err(|log| format!("no `{line}` on stdout:\n{log}"))
    }

    /// The port from the program's latest `listening on ...<port>` line.
    pub fn port(&self) -> Result<u16, String> {
        let mut port = None;
        self.wait(|log| {
            port = log
                .stdout
                .iter()
                .rev()
                .find_map(|(_, l)| l.strip_prefix("listening on "))
                .and_then(|rest| rest.rsplit([':', ' ']).next()?.parse().ok());
            port.is_some()
        })
        .map_err(|log| format!("the program printed no `listening on <port>`:\n{log}"))?;
        Ok(port.unwrap_or_default())
    }

    /// Poll the log until `done` holds; on timeout return the whole log.
    fn wait(&self, mut done: impl FnMut(&Log) -> bool) -> Result<(), String> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            {
                let log = self.log.lock().unwrap();
                if done(&log) {
                    return Ok(());
                }
                if Instant::now() > deadline {
                    return Err(format!(
                        "--- stdout ---\n{}\n--- stderr ---\n{}",
                        text(&log.stdout),
                        text(&log.stderr)
                    ));
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Dev {
    /// End `velt dev` and its programs, and wait until they have exited: on Windows their
    /// directory can't be removed while they hold files in it.
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.child.id()) {
            // SAFETY: signals our own process group (the supervisor and the program).
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        #[cfg(windows)]
        let ended = self.job.end();
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(windows)]
        if !ended && !std::thread::panicking() {
            panic!("the programs `velt dev` started did not exit");
        }
    }
}

/// Captured lines as text.
fn text(lines: &[(Instant, String)]) -> String {
    let lines: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
    lines.join("\n")
}

fn capture(
    stream: impl Read + Send + 'static,
    log: Arc<Mutex<Log>>,
    lines: fn(&mut Log) -> &mut Vec<(Instant, String)>,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            lines(&mut log.lock().unwrap()).push((Instant::now(), line));
        }
    });
}

/// `GET path` on 127.0.0.1:`port`; the response body, or why there is none.
pub fn get(port: u16, path: &str) -> Result<String, String> {
    request(port, path).map_err(|e| format!("{:?}: {e}", e.kind()))
}

fn request(port: u16, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    // Long enough for a slow handler left running across a reload (tests/reload/in_flight).
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    match response.split_once("\r\n\r\n") {
        Some((_, body)) => Ok(body.to_string()),
        None => Err(std::io::Error::other(format!("no body in `{response}`"))),
    }
}

/// A `GET` left running: its body (or why there is none) and when it arrived.
pub struct Background(JoinHandle<(Instant, Result<String, String>)>);

impl Background {
    /// Start `GET path` on another thread.
    pub fn start(port: u16, path: &str) -> Background {
        let path = path.to_string();
        Background(std::thread::spawn(move || {
            let body = get(port, &path);
            (Instant::now(), body)
        }))
    }

    /// Wait for the response.
    pub fn finish(self) -> (Instant, Result<String, String>) {
        self.0
            .join()
            .unwrap_or_else(|_| (Instant::now(), Err("the request thread panicked".into())))
    }
}

/// A client that keeps requesting `/` until stopped and counts refused connections.
pub struct Probe {
    stop: Arc<AtomicBool>,
    refused: Arc<AtomicUsize>,
    thread: JoinHandle<()>,
}

impl Probe {
    /// Start probing `port`.
    pub fn start(port: u16) -> Probe {
        let stop = Arc::new(AtomicBool::new(false));
        let refused = Arc::new(AtomicUsize::new(0));
        let (s, r) = (stop.clone(), refused.clone());
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                if let Err(e) = request(port, "/") {
                    if e.kind() == std::io::ErrorKind::ConnectionRefused {
                        r.fetch_add(1, Ordering::Relaxed);
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        Probe {
            stop,
            refused,
            thread,
        }
    }

    /// Stop; the number of refused connections.
    pub fn stop(self) -> usize {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
        self.refused.load(Ordering::Relaxed)
    }
}

/// A temporary directory for a `velt dev` session, removed when dropped. `tempfile` ignores a
/// removal that fails, which on Windows left a directory behind whenever a program still held
/// a file in it (the shared runtime is about 110 MB with its debug info); here a removal that
/// still fails after the files were released ([`remove`]) fails the test. Drop the [`Dev`] using
/// it first.
pub struct TestDir(Option<tempfile::TempDir>);

impl TestDir {
    /// A new directory in the system's temporary directory, named `velt-dev-test-*`.
    pub fn new() -> TestDir {
        let dir = tempfile::Builder::new().prefix("velt-dev-test-").tempdir();
        TestDir(Some(dir.expect("temp dir")))
    }

    /// Where it is.
    pub fn path(&self) -> &Path {
        self.0.as_ref().expect("ICE: removed").path()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let Some(dir) = self.0.take() else { return };
        let path = dir.keep();
        if let Err(e) = remove(&path) {
            let msg = format!("cannot remove the test directory {}: {e}", path.display());
            if std::thread::panicking() {
                eprintln!("{msg}");
            } else {
                panic!("{msg}");
            }
        }
    }
}

/// How long Windows may take to release the files of programs that have exited.
#[cfg(windows)]
const RELEASE_LIMIT: Duration = Duration::from_secs(10);

/// Remove `dir` and everything in it. On Windows the executable and DLLs of a program that has
/// exited, and been waited for, can stay locked for a few more milliseconds (the system tears
/// down the image mapping, an antivirus scans the closed files): a removal that fails is
/// repeated until it succeeds or [`RELEASE_LIMIT`] has passed.
fn remove(dir: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let deadline = Instant::now() + RELEASE_LIMIT;
        loop {
            match std::fs::remove_dir_all(dir) {
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => return result,
            }
        }
    }
    #[cfg(not(windows))]
    std::fs::remove_dir_all(dir)
}
