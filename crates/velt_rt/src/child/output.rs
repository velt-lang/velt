//! Run a command to completion and collect its output (`exec`/`execSync` in std): stdout and
//! stderr are piped and read concurrently (so neither pipe can fill up and deadlock the child),
//! optional input is written to stdin, and the result carries the exit code and both outputs
//! as text (invalid UTF-8 becomes U+FFFD).

use super::command::{StdioMode, VeltCommand};
use super::{exit_code, spawn_error};
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use std::io::Write;
use std::process::Output;
use tokio::io::AsyncWriteExt;

/// `{ i32 code; u32 pad; VeltStr stdout; VeltStr stderr; }` — size 56, align 8.
#[repr(C)]
pub struct VeltOutput {
    /// Exit code (`128 + signal` when killed by a signal).
    pub code: i32,
    /// Always 0.
    pub pad: u32,
    /// Everything the child wrote to stdout.
    pub stdout: VeltStr,
    /// Everything the child wrote to stderr.
    pub stderr: VeltStr,
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

/// `exec(cmd, args, opts)` → `IoResult<VeltOutput>` once the child has exited. `input` (a
/// string, copied; empty = none) is written to its stdin, which is then closed. The spec is
/// read at the call.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_output(
    spec: *const VeltCommand,
    input: *const VeltStr,
) -> *mut VeltFut {
    let input = (*input).to_string_lossy().into_bytes();
    let cmd = command(&*spec, &input);
    let program = VeltStr::from_bytes((*spec).program.as_bytes());
    new_leaf(async move {
        let r = run(tokio::process::Command::from(cmd), input).await;
        let r = r.map(to_output).map_err(|e| spawn_error(&program, &e));
        let mut program = program;
        crate::str::velt_rt_str_drop(&mut program);
        match r {
            Ok(out) => IoResult::ok(out),
            Err(e) => IoResult::err(e),
        }
    })
}

async fn run(mut cmd: tokio::process::Command, input: Vec<u8>) -> std::io::Result<Output> {
    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // Fed from its own task: a child that writes a lot before reading its input must not
        // block on a full stdout pipe while we block writing to its stdin.
        tokio::spawn(async move {
            let _ = stdin.write_all(&input).await;
        });
    }
    child.wait_with_output().await
}

/// `execSync`: like `velt_rt_child_output`, blocking the calling thread.
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
