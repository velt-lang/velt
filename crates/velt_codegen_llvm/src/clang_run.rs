//! Running a candidate clang with a time limit (`clang_probe`, the CPU macros of `clang`).

use std::io::{Read, Write};
use std::process::{Command, ExitStatus};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::clang_probe::SMALLEST_IR;

/// How much of a candidate's output is kept: its first line is all that is wanted. The rest is read
/// and dropped, so that it isn't killed by a closed pipe, whatever it writes.
pub(crate) const MOST_OUTPUT: u64 = 64 * 1024;

/// What came of running a program with a limit.
pub(crate) enum Ran {
    /// Its status, and what it wrote to stdout and to stderr (each empty unless piped).
    Exited(ExitStatus, Vec<u8>, Vec<u8>),
    NotStarted(String),
    TimedOut,
}

/// Run `cmd` and give it [`SMALLEST_IR`] if stdin is piped. What it writes to a piped stdout or
/// stderr is read, up to [`MOST_OUTPUT`] each. A child that is still running at `limit` is killed;
/// a process it started may live on, and the threads reading their pipes with it, until this
/// process exits.
pub(crate) fn run_limited(mut cmd: Command, limit: Duration) -> Ran {
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return Ran::NotStarted(e.to_string()),
    };
    if let Some(mut stdin) = child.stdin.take() {
        // It fits a pipe's buffer, so this doesn't block. One that exits unread is judged by that.
        let _ = stdin.write_all(SMALLEST_IR);
    }
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ];
    let (send, receive) = mpsc::channel();
    let mut reading = 0;
    for (which, pipe) in pipes.into_iter().enumerate() {
        let Some(mut pipe) = pipe else { continue };
        reading += 1;
        let send = send.clone();
        std::thread::spawn(move || {
            let mut text = vec![];
            let _ = pipe.by_ref().take(MOST_OUTPUT).read_to_end(&mut text);
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
            let _ = send.send((which, text));
        });
    }
    let deadline = Instant::now() + limit;
    let mut texts = [vec![], vec![]];
    let mut closed = 0;
    while closed < reading {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok((which, text)) = receive.recv_timeout(left) else {
            break;
        };
        texts[which] = text;
        closed += 1;
    }
    // With the pipes closed the child is done or nearly: `wait` could still block on one that
    // closed them and hangs.
    let finished = (closed == reading).then(|| loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            _ => return None,
        }
    });
    match finished.flatten() {
        Some(status) => {
            let [out, err] = texts;
            Ran::Exited(status, out, err)
        }
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Ran::TimedOut
        }
    }
}
