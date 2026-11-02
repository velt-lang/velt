//! The generator host as a child process: how long it may run, how much it may write, and that
//! it never outlives the compiler (a language server that exits while a host runs stops it too).

use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one run of a generator host may take, in seconds (`0`: no limit).
pub(super) const TIMEOUT_ENV: &str = "VELT_GENERATOR_TIMEOUT";

/// The limit without [`TIMEOUT_ENV`].
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// How much a host may write to its standard output, and to its standard error.
const OUTPUT_LIMIT: u64 = 128 << 20;

/// Why a host run gave no output.
pub(super) enum RunError {
    /// It did not build, did not start or failed: the message, with the host's errors.
    Failed(String),
    /// It ran longer than the limit and was stopped.
    TimedOut(Duration),
}

/// The time a host run may take: [`TIMEOUT_ENV`], else [`DEFAULT_TIMEOUT`]; `None`: no limit.
fn timeout() -> Option<Duration> {
    let v = std::env::var_os(TIMEOUT_ENV);
    let v = v.as_ref().map(|v| v.to_str().unwrap_or("?"));
    parse_timeout(v).unwrap_or_else(|()| {
        crate::style::warning(&format!(
            "{TIMEOUT_ENV} must be a number of seconds; using {}",
            show_duration(DEFAULT_TIMEOUT)
        ));
        Some(DEFAULT_TIMEOUT)
    })
}

/// [`TIMEOUT_ENV`]'s value `v`: the limit (`None`: none, for `0` and for a time too long to
/// wait for), or `Err` when it is not a number of seconds.
fn parse_timeout(v: Option<&str>) -> Result<Option<Duration>, ()> {
    let Some(v) = v else {
        return Ok(Some(DEFAULT_TIMEOUT));
    };
    match v.trim().parse::<f64>() {
        Ok(0.0) => Ok(None),
        Ok(s) if s > 0.0 && s.is_finite() => Ok(Duration::try_from_secs_f64(s).ok()),
        _ => Err(()),
    }
}

/// When a run started now with limit `limit` must end, with the limit; `None`: no limit (also
/// for one too long to tell the end of).
fn deadline(limit: Option<Duration>) -> Option<(Instant, Duration)> {
    let limit = limit?;
    Some((Instant::now().checked_add(limit)?, limit))
}

/// `30 s`, `1.5 s` or `200 ms`.
pub(super) fn show_duration(d: Duration) -> String {
    let ms = d.as_millis();
    match ms {
        _ if ms < 1000 => format!("{ms} ms"),
        _ if ms.is_multiple_of(1000) => format!("{} s", ms / 1000),
        _ => format!("{} s", ms as f64 / 1000.0),
    }
}

/// What the readers of a host's output report.
enum Event {
    /// Its standard output ended: all of it.
    Out(Vec<u8>),
    /// It wrote more than [`OUTPUT_LIMIT`] to the stream named.
    TooMuch(&'static str),
}

/// Run `command` (a host) on `input` and return its standard output. A host that runs longer
/// than [`timeout`], or writes more than [`OUTPUT_LIMIT`] to its standard output or error, is
/// killed.
pub(super) fn run(command: &mut Command, input: &str) -> Result<Vec<u8>, RunError> {
    let failed = RunError::Failed;
    let deadline = deadline(timeout());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    tie_to_this_process(command);
    let mut child =
        (command.spawn()).map_err(|e| failed(format!("cannot start the generator host: {e}")))?;
    let tracked = Tracked::new(&child);
    let (Some(mut stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        stop(&mut child, tracked);
        return Err(failed("the generator host has no standard streams".into()));
    };
    let input = input.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let (tx, rx) = mpsc::channel();
    let too_much = tx.clone();
    let errs = std::thread::spawn(move || {
        let (err, over) = read_capped(stderr);
        if over {
            let _ = too_much.send(Event::TooMuch("error"));
        }
        err
    });
    std::thread::spawn(move || {
        let (out, over) = read_capped(stdout);
        let _ = tx.send(if over {
            Event::TooMuch("output")
        } else {
            Event::Out(out)
        });
    });
    // Its output ends when it exits (or closes it); then it is waited for, within what is
    // left of the limit.
    let event = match deadline {
        Some((at, _)) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
        None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
    };
    let out = match event {
        Ok(Event::TooMuch(stream)) => {
            stop(&mut child, tracked);
            let _ = (writer.join(), errs.join());
            return Err(failed(format!(
                "the generator host wrote more than {} MB to its standard {stream}, and was stopped",
                OUTPUT_LIMIT >> 20
            )));
        }
        Ok(Event::Out(out)) => Some(out),
        Err(RecvTimeoutError::Disconnected) => None,
        Err(RecvTimeoutError::Timeout) => {
            stop(&mut child, tracked);
            let _ = (writer.join(), errs.join());
            let limit = deadline.map_or(DEFAULT_TIMEOUT, |(_, t)| t);
            return Err(RunError::TimedOut(limit));
        }
    };
    let Some(status) = wait_until(&mut child, deadline) else {
        stop(&mut child, tracked);
        let _ = (writer.join(), errs.join());
        let limit = deadline.map_or(DEFAULT_TIMEOUT, |(_, t)| t);
        return Err(RunError::TimedOut(limit));
    };
    drop(tracked);
    let status = status.map_err(|e| failed(e.to_string()))?;
    let _ = writer.join();
    let err = errs.join().unwrap_or_default();
    if !status.success() {
        let err = String::from_utf8_lossy(&err);
        return Err(failed(format!(
            "the generator host exited with {status}:\n{}",
            err.trim_end()
        )));
    }
    Ok(out.unwrap_or_default())
}

/// All of `r` up to [`OUTPUT_LIMIT`] bytes, and whether it had more.
fn read_capped(r: impl Read) -> (Vec<u8>, bool) {
    let mut out = vec![];
    let _ = r.take(OUTPUT_LIMIT + 1).read_to_end(&mut out);
    let over = out.len() as u64 > OUTPUT_LIMIT;
    out.truncate(OUTPUT_LIMIT as usize);
    (out, over)
}

/// Kill `child` and reap it.
fn stop(child: &mut Child, tracked: Tracked) {
    let _ = child.kill();
    let _ = child.wait();
    drop(tracked);
}

/// The exit status of `child`, waiting at most until `deadline`; `None` when it runs past it.
fn wait_until(
    child: &mut Child,
    deadline: Option<(Instant, Duration)>,
) -> Option<std::io::Result<ExitStatus>> {
    let Some((at, _)) = deadline else {
        return Some(child.wait());
    };
    // Its output has ended, so it is exiting: poll briefly.
    let mut pause = Duration::from_millis(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(Ok(status)),
            Ok(None) if Instant::now() >= at => return None,
            Ok(None) => {}
            Err(e) => return Some(Err(e)),
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

/// The hosts running now (their process ids), for [`stop_all`].
static RUNNING: Mutex<Vec<u32>> = Mutex::new(vec![]);

/// A running host, in [`RUNNING`] until it is dropped (after the host was reaped).
struct Tracked(u32);

impl Tracked {
    fn new(child: &Child) -> Tracked {
        #[cfg(windows)]
        crate::dev::job::kill_with_supervisor(child);
        let id = child.id();
        RUNNING.lock().unwrap_or_else(|e| e.into_inner()).push(id);
        Tracked(id)
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        running.retain(|id| *id != self.0);
    }
}

/// Kill every host running now: the language server calls this when it exits, so that a host
/// it started (a run the user never asked for) does not run on. On Linux a host also dies with
/// its parent (`PR_SET_PDEATHSIG`), and on Windows with this process (a kill-on-close job).
pub(crate) fn stop_all() {
    let running = std::mem::take(&mut *RUNNING.lock().unwrap_or_else(|e| e.into_inner()));
    #[cfg(unix)]
    for id in running {
        if let Ok(pid) = libc::pid_t::try_from(id) {
            // SAFETY: a plain signal to a host this process started and has not reaped.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
    #[cfg(not(unix))]
    drop(running);
}

/// A host dies with this process: on Linux by `PR_SET_PDEATHSIG` (it also stops a nested host
/// whose JIT parent dies), on Windows by the job [`Tracked::new`] puts it in, where it also
/// gets no console window (a host the language server starts).
fn tie_to_this_process(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `prctl` is async-signal-safe and touches no memory of the parent.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts() {
        let secs = |s: f64| Ok(Some(Duration::from_secs_f64(s)));
        assert_eq!(parse_timeout(None), Ok(Some(DEFAULT_TIMEOUT)));
        assert_eq!(parse_timeout(Some("0.5")), secs(0.5));
        assert_eq!(parse_timeout(Some(" 2 ")), secs(2.0));
        assert_eq!(parse_timeout(Some("0")), Ok(None));
        // Too long to wait for: no limit.
        assert_eq!(parse_timeout(Some("1e30")), Ok(None));
        // Past what `Instant` can hold on Linux (no deadline); on Windows it can, and the
        // deadline is billions of years away. Never reached either way.
        let far = deadline(parse_timeout(Some("1e19")).unwrap());
        let year = Duration::from_secs(365 * 24 * 3600);
        assert!(far.is_none_or(|(at, _)| at > Instant::now() + 1000 * year));
        for bad in ["abc", "-1", "inf", "NaN", ""] {
            assert_eq!(parse_timeout(Some(bad)), Err(()), "{bad}");
        }
    }

    #[test]
    fn durations() {
        assert_eq!(show_duration(Duration::from_secs(30)), "30 s");
        assert_eq!(show_duration(Duration::from_millis(1500)), "1.5 s");
        assert_eq!(show_duration(Duration::from_millis(500)), "500 ms");
    }
}
