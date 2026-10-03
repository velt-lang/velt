//! `std/child_process`: running other programs.
//!
//! `spawn` starts a child with configurable stdio (inherit / pipe / ignore) and returns an opaque
//! handle (a key into a handle table, like TCP streams: in-flight operations hold their own
//! `Arc`, so the handle can be released at any time, and a released one fails with `EBADF`).
//! A background task owns the OS child: it reaps it (no zombies even if nobody waits),
//! publishes the exit code, and performs kills on platforms without signals.
//! Pipe reads and writes are `VeltFut`s (`pipes.rs`); `output`/`output_sync` run a command to
//! completion and collect its output (`output.rs`).
//!
//! Nothing here stores callbacks: progress is observed by awaiting futures (§13.5).

mod command;
mod output;
mod pipes;

use crate::registry::{closed, closed_error, Key, Registry};
use crate::result::{IoResult, VeltErr};
use crate::str::VeltStr;
use command::{StdioMode, VeltCommand};
use futures_util::future::Either;
use pipes::Reader;
use std::sync::Arc;
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::{watch, Mutex, Notify};

/// A running (or finished) child process.
pub struct ChildObj {
    pid: u32,
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Mutex<Option<Reader<ChildStdout>>>,
    stderr: Mutex<Option<Reader<ChildStderr>>>,
    /// `None` while running, then the exit code (see [`exit_code`]).
    status: watch::Receiver<Option<Result<i32, i32>>>,
    /// Asks the reaper task to kill the child (platforms without `kill(2)`).
    kill: Arc<Notify>,
}

/// Opaque child handle.
pub type ChildHandle = Key<ChildObj>;

pub(crate) static CHILDREN: Registry<ChildObj> = Registry::new();

/// The exit code of a finished process; killed by a signal ⇒ `128 + signal` (shell convention).
fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    -1
}

/// Prefixes spawn errors with the program, like Node's `spawn x ENOENT`.
fn spawn_error(program: &VeltStr, e: &std::io::Error) -> VeltErr {
    // SAFETY: `program` is a valid string borrowed from the caller.
    let name = String::from_utf8_lossy(unsafe { program.as_bytes() });
    let mut err = VeltErr::from_io(e);
    let message = format!("spawn {name}: {e}");
    // SAFETY: `err.message` is an owned string created just above.
    unsafe { crate::str::velt_rt_str_drop(&mut err.message) };
    err.message = VeltStr::from_vec(message.into_bytes());
    err
}

fn start(spec: &VeltCommand) -> Result<ChildObj, VeltErr> {
    let modes = spec.modes();
    // A child writing to our stdout or stderr must come after what we printed before starting
    // it: our buffered stdout has to reach the OS first (stderr is unbuffered, but a stderr
    // write is ordered after earlier stdout output too).
    if modes[1] == StdioMode::Inherit || modes[2] == StdioMode::Inherit {
        crate::io::flush_stdout();
    }
    // SAFETY: the spec is borrowed from Velt for the duration of the call.
    let cmd = unsafe { spec.build(modes) };
    // Spawning registers the child with the reactor: it needs the runtime context.
    let _guard = crate::task::runtime::handle().enter();
    let mut child = tokio::process::Command::from(cmd)
        .spawn()
        .map_err(|e| spawn_error(&spec.program, &e))?;
    let (tx, rx) = watch::channel(None);
    let kill = Arc::new(Notify::new());
    let obj = ChildObj {
        pid: child.id().unwrap_or(0),
        stdin: Mutex::new(child.stdin.take()),
        stdout: Mutex::new(child.stdout.take().map(Reader::new)),
        stderr: Mutex::new(child.stderr.take().map(Reader::new)),
        status: rx,
        kill: kill.clone(),
    };
    tokio::spawn(async move {
        let status = loop {
            let killed = {
                let wait = std::pin::pin!(child.wait());
                let notified = std::pin::pin!(kill.notified());
                match futures_util::future::select(wait, notified).await {
                    Either::Left((r, _)) => break r,
                    Either::Right(_) => true,
                }
            };
            if killed {
                let _ = child.start_kill();
            }
        };
        let _ = tx.send(Some(
            status.map(exit_code).map_err(|e| VeltErr::from_io(&e).code),
        ));
    });
    Ok(obj)
}

/// `spawn(cmd, args, opts)` → `IoResult<VeltChild*>` (no return value: std reads the code from `out`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_spawn(
    spec: *const VeltCommand,
    out: *mut IoResult<ChildHandle>,
) {
    let r = match start(&*spec) {
        Ok(obj) => IoResult::ok(CHILDREN.insert(obj)),
        Err(e) => IoResult::err(e),
    };
    out.write(r);
}

/// The OS process id (0 once the handle is closed).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_pid(c: ChildHandle) -> u32 {
    CHILDREN.get(c).map_or(0, |o| o.pid)
}

/// The exit code if the child has finished, else -1 (never blocks; also -1 once closed).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_exit_code(c: ChildHandle) -> i64 {
    let Some(obj) = CHILDREN.get(c) else {
        return -1;
    };
    let code = *obj.status.borrow();
    match code {
        Some(Ok(code)) => code as i64,
        _ => -1,
    }
}

/// `wait()` → `IoResult<i32>`: the exit code once the child has finished.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_wait(c: ChildHandle) -> *mut crate::task::VeltFut {
    let Some(obj) = CHILDREN.get(c) else {
        return crate::task::leaf::new_leaf(async { closed::<i32>() });
    };
    let mut status = obj.status.clone();
    crate::task::leaf::new_leaf(async move {
        let r = status.wait_for(Option::is_some).await.map(|s| *s);
        match r {
            Ok(Some(Ok(code))) => IoResult::ok(code),
            Ok(Some(Err(code))) => {
                IoResult::err(VeltErr::new(code, "waiting for the child failed"))
            }
            _ => IoResult::err(VeltErr::new(
                crate::result::code::OTHER,
                "child reaper stopped",
            )),
        }
    })
}

/// Send `signal` (a Unix signal number; on Windows every signal terminates the process).
/// Signalling a finished child does nothing. `out` receives the status.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_kill(c: ChildHandle, signal: i32, out: *mut VeltErr) {
    let Some(obj) = CHILDREN.get(c) else {
        return out.write(closed_error());
    };
    let err = if obj.status.borrow().is_some() {
        VeltErr::ok()
    } else {
        send_signal(&obj, signal)
    };
    out.write(err);
}

#[cfg(unix)]
fn send_signal(obj: &ChildObj, signal: i32) -> VeltErr {
    if signal == libc::SIGKILL {
        // The reaper kills through tokio, which also covers a child it is about to reap.
        obj.kill.notify_one();
        return VeltErr::ok();
    }
    // The reaper has not published an exit, so the pid still belongs to our (maybe zombie) child.
    // SAFETY: plain syscall.
    if unsafe { libc::kill(obj.pid as libc::pid_t, signal) } == 0 {
        VeltErr::ok()
    } else {
        VeltErr::from_io(&std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn send_signal(obj: &ChildObj, _signal: i32) -> VeltErr {
    obj.kill.notify_one();
    VeltErr::ok()
}

/// Release a handle (a no-op when already closed through any copy). The child keeps running
/// (it is reaped in the background).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_close(c: ChildHandle) {
    CHILDREN.remove(c);
}

// The child-process tests spawn unix tools (sh, cat); Windows is covered by the goldens.
#[cfg(all(test, unix))]
mod tests;
