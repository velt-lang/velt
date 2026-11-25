//! Run a command to completion and collect its output, blocking the calling thread (`execSync`
//! in std on Windows and WebAssembly; macOS and Linux run it in std, exec_sync_posix.velt):
//! stdout and stderr are piped and read concurrently (so neither pipe can fill up and deadlock
//! the child), optional input is written to stdin from a thread of its own, and the result
//! carries the exit code and both outputs as text (invalid UTF-8 becomes U+FFFD).

use super::command::{StdioMode, VeltCommand};
use crate::result::{IoResult, VeltErr};
use crate::str::VeltStr;
use std::io::Write;
use std::process::Output;

/// `{ i32 code; u32 pad; VeltStr stdout; VeltStr stderr; }` — size 56, align 8.
#[repr(C)]
pub struct VeltOutput {
    /// Exit code (Windows' unsigned code as its bits; -1 when there is none).
    pub code: i32,
    /// Always 0.
    pub pad: u32,
    /// Everything the child wrote to stdout.
    pub stdout: VeltStr,
    /// Everything the child wrote to stderr.
    pub stderr: VeltStr,
}

/// The exit code of a finished process (-1 when it has none). Only Windows and WebAssembly come
/// here, where a process always has one.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// Prefixes spawn errors with the program, like Node's `spawn x ENOENT`.
fn spawn_error(program: &VeltStr, e: &std::io::Error) -> VeltErr {
    // SAFETY: `program` is a valid string borrowed from the caller.
    let name = unsafe { program.text_lossy() };
    let mut err = VeltErr::from_io(e);
    let message = format!("spawn {name}: {e}");
    // SAFETY: `err.message` is an owned string created just above.
    unsafe { crate::str::velt_rt_str_drop(&mut err.message) };
    err.message = VeltStr::from_vec(message.into_bytes());
    err
}

fn text(bytes: Vec<u8>) -> VeltStr {
    match String::from_utf8(bytes) {
        Ok(s) => VeltStr::from_vec(s.into_bytes()),
        Err(e) => VeltStr::from_vec(
            String::from_utf8_lossy(e.as_bytes())
                .into_owned()
                .into_bytes(),
        ),
    }
}

fn to_output(out: Output) -> VeltOutput {
    VeltOutput {
        code: exit_code(out.status),
        pad: 0,
        stdout: text(out.stdout),
        stderr: text(out.stderr),
    }
}

/// Stdin is a pipe only when there is input; the child reads end of stream otherwise.
unsafe fn command(spec: &VeltCommand, input: &[u8]) -> std::process::Command {
    let stdin = if input.is_empty() {
        StdioMode::Ignore
    } else {
        StdioMode::Pipe
    };
    spec.build([stdin, StdioMode::Pipe, StdioMode::Pipe])
}

/// `execSync(cmd, args, opts)` → `IoResult<VeltOutput>` once the child has exited. `input` (a
/// string, copied; empty = none) is written to its stdin, which is then closed.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_output_sync(
    spec: *const VeltCommand,
    input: *const VeltStr,
    out: *mut IoResult<VeltOutput>,
) {
    let input = (*input).to_string_lossy().into_bytes();
    let r = run_sync(command(&*spec, &input), input);
    let r = match r {
        Ok(o) => IoResult::ok(to_output(o)),
        Err(e) => IoResult::err(spawn_error(&(*spec).program, &e)),
    };
    out.write(r);
}

fn run_sync(mut cmd: std::process::Command, input: Vec<u8>) -> std::io::Result<Output> {
    let mut child = cmd.spawn()?;
    let feeder = child.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        })
    });
    let out = child.wait_with_output();
    if let Some(feeder) = feeder {
        let _ = feeder.join();
    }
    out
}

const _: () = assert!(std::mem::size_of::<VeltOutput>() == 56);
