//! Hot swap in the JIT host (docs/internals/design/hot-reload.md, phase 3): on each `reload` request the
//! host builds the current sources while the program keeps running, and `DevSession::reload`
//! either swaps the changed functions in or says why the program must restart. After a swap,
//! HTTP servers whose handler was recompiled get a descriptor with the new code, so their new
//! requests run it (in-flight requests finish on the code they started with).

use std::time::Instant;

use velt_codegen_cl::{DevSession, Reload};
use velt_rt_host::dev::handover::reload::{reply_reload, wait_reload, Reloaded};
use velt_rt_host::dev::handover::Stream;
use velt_rt_host::http::handler::{update_handlers, InitFn, VeltHandler};
use velt_rt_host::task::{DropFn, PollFn};

use super::native::{source_files, Fingerprint};
use crate::commands::{failure_code, report};
use crate::driver::{self, BuildOptions, Session};

/// Answer reload requests on `channel` on a background thread for the life of the program.
pub fn serve_reloads(channel: Stream, opts: BuildOptions, mut session: DevSession, verbose: bool) {
    let natives = Fingerprint::of(opts.packages.as_ref());
    let spawned = std::thread::Builder::new()
        .name("velt-dev-reload".into())
        .spawn(move || {
            while wait_reload(&channel).is_ok() {
                let reply = if Fingerprint::of(opts.packages.as_ref()) != natives {
                    // Native libraries are never swapped (or unloaded): start over.
                    Reloaded::Restart {
                        reason: "a native library changed".into(),
                        files: source_files(opts.packages.as_ref()),
                    }
                } else {
                    reload(&mut session, &opts, verbose)
                };
                if reply_reload(&channel, &reply).is_err() {
                    break;
                }
            }
        });
    if let Err(e) = spawned {
        // Without the thread the supervisor's reload request fails and it restarts instead.
        crate::style::error(&format!("velt dev: cannot start the reload thread: {e}"));
    }
}

/// Build the current sources and take them into the running program if possible.
fn reload(session: &mut DevSession, opts: &BuildOptions, verbose: bool) -> Reloaded {
    let mut sess = Session::new();
    let program = driver::compile(&mut sess, opts);
    let mut files: Vec<_> = sess.sm.files().map(|(_, f)| f.path.clone()).collect();
    files.extend(source_files(opts.packages.as_ref()));
    let program = match program {
        Ok(program) => program,
        Err(err) => {
            report(&sess, verbose);
            failure_code(&err);
            return Reloaded::Failed { files };
        }
    };
    let start = Instant::now();
    let outcome = session.reload(&program);
    sess.timings.push(("jit", start.elapsed()));
    report(&sess, verbose);
    match outcome {
        Ok(Reload::Swapped { functions }) => {
            update_handlers(|desc| handler_update(session, desc));
            Reloaded::Swapped { functions, files }
        }
        Ok(Reload::Restart(reason)) => Reloaded::Restart { reason, files },
        Err(e) => Reloaded::Restart {
            reason: format!("code generation failed: {e}"),
            files,
        },
    }
}

/// The descriptor a running server should switch to, if the swap recompiled its handler.
fn handler_update(session: &DevSession, desc: &VeltHandler) -> Option<VeltHandler> {
    let code = session.handler_update(desc.init as usize)?;
    // SAFETY: the addresses are the newest JIT code of this handler's `init`, `$poll` and
    // `$drop`, compiled with exactly the C signatures of these descriptor fields
    // (rt_abi_async.md §7), and never freed during the session.
    unsafe {
        Some(VeltHandler {
            init: std::mem::transmute::<usize, InitFn>(code.init),
            poll: std::mem::transmute::<usize, PollFn>(code.poll),
            drop: std::mem::transmute::<usize, DropFn>(code.drop),
            state_size: code.state_size,
            state_align: code.state_align,
            env: desc.env,
        })
    }
}

#[cfg(test)]
mod tests;
