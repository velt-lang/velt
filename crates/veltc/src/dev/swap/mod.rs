//! Hot swap in the JIT host (docs/internals/design/hot-reload.md, phase 3): on each `reload` request the
//! host builds the current sources while the program keeps running, and `DevSession::reload`
//! either swaps the changed functions in or says why the program must restart. New calls reach
//! the new code through the trampolines (an HTTP server's next request runs the new handler);
//! a future in flight finishes on the code of the version that started it.

use std::time::Instant;

use velt_codegen_cl::{DevSession, Reload};
use velt_rt_host::dev::handover::reload::{reply_reload, wait_reload, Reloaded};
use velt_rt_host::dev::handover::Stream;

use super::native::{source_files, Fingerprint};
use crate::cli::BuildArgs;
use crate::commands::{failure_code, report};
use crate::driver::{self, BuildOptions, Session};

/// For tests: a host with this set crashes (aborts) when asked to reload, as one whose swap
/// faults would.
pub const CRASH_ON_RELOAD_ENV: &str = "VELT_DEV_CRASH_ON_RELOAD";

/// Answer reload requests on `channel` on a background thread for the life of the program;
/// `args` says what to report (`-v`, `--timings`).
pub fn serve_reloads(
    channel: Stream,
    opts: BuildOptions,
    mut session: DevSession,
    mut carried: Carried,
    args: &BuildArgs,
) {
    let flags = Report {
        verbose: args.verbose,
        details: args.timings,
    };
    let natives = Fingerprint::of(opts.packages.as_ref());
    let crash = std::env::var_os(CRASH_ON_RELOAD_ENV).is_some();
    let spawned = std::thread::Builder::new()
        .name("velt-dev-reload".into())
        .spawn(move || {
            while wait_reload(&channel).is_ok() {
                if crash {
                    std::process::abort();
                }
                let reply = if Fingerprint::of(opts.packages.as_ref()) != natives {
                    // Native libraries are never swapped (or unloaded): start over.
                    Reloaded::Restart {
                        reason: "a native library changed".into(),
                        files: source_files(opts.packages.as_ref()),
                    }
                } else {
                    reload(&mut session, &opts, flags, &mut carried)
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

/// What a reload prints about its build.
#[derive(Clone, Copy)]
struct Report {
    /// `-v`: per-stage timings.
    verbose: bool,
    /// `--timings`: each stage's breakdown too.
    details: bool,
}

/// What a reload takes from the running version, and leaves for the next one.
pub struct Carried {
    /// Lowering's counted types (velt_vir `lower_seeded`).
    pub seed: Option<velt_vir::CountedSeed>,
    /// The program's own definitions (`super::edits`): what the next save changed.
    pub own: Option<super::edits::Snapshot>,
    /// The checked program's interfaces (velt_sema `interface`), kept with `-v` to say whether
    /// a body edit kept everything else's.
    pub interfaces: Option<velt_sema::interface::Summary>,
    /// With `-v`, the last good build's kept check (velt_sema `KeptCheck`,
    /// dev-warm-front-end.md step 3 "C2"): kept and measured, not used yet.
    pub kept: Option<velt_sema::KeptCheck>,
}

/// What `-v` says of a kept check: its size, and the process's memory with it kept.
pub fn kept_report(kept: &velt_sema::KeptCheck) -> String {
    let memory = crate::driver::memory::read().map(crate::driver::memory::render);
    format!(
        "kept the check: {} modules, {} definitions{}",
        kept.module_paths().len(),
        kept.definitions(),
        memory.map(|m| format!(" ({m})")).unwrap_or_default()
    )
}

/// Build the current sources and take them into the running program if possible.
/// `carried`: what the running version leaves; a version that represents a value of the running
/// one differently restarts.
fn reload(
    session: &mut DevSession,
    opts: &BuildOptions,
    flags: Report,
    carried: &mut Carried,
) -> Reloaded {
    let load = |sess: &Session| super::native::load_c_libraries(opts, &sess.c_libs);
    reload_with(session, opts, flags, carried, load)
}

/// [`reload`], with `load` taking in what the new version needs after it built (its C libraries).
fn reload_with(
    session: &mut DevSession,
    opts: &BuildOptions,
    flags: Report,
    carried: &mut Carried,
    load: impl FnOnce(&Session) -> Result<(), String>,
) -> Reloaded {
    let verbose = flags.verbose;
    let mut sess = Session::new();
    sess.show_details = flags.details;
    // The running version's counted types are what live values were made with: they stay the
    // baseline until a version is taken in (swapped in, or restarted into). A version that fails
    // after lowering never ran, so its seed must not become the next comparison's baseline.
    let running = carried.seed.take().unwrap_or_default();
    sess.counted_seed = Some(running.clone());
    sess.keep_check = verbose;
    sess.interfaces = verbose.then(Default::default);
    let program = driver::compile_for_jit(&mut sess, opts);
    let next = sess.counted_seed.take();
    // Values the running code made that the new code would count, free or call differently
    // (a class that is counted in one version only, #2653): only a restart is safe.
    let changed = (next.as_ref()).and_then(|next| running.representation_change(next));
    if program.is_ok() {
        let own = super::edits::Snapshot::of_build(&sess, &opts.input);
        let interfaces = sess.interfaces.take();
        if let (true, Some(before)) = (verbose, &carried.own) {
            let edit = super::edits::classify(before, &own);
            eprintln!("velt dev: edit: {edit}");
            if let (Some(a), Some(b)) = (&carried.interfaces, &interfaces) {
                if let Some(said) = super::edits::cutoff(&edit, &a.changes(b)) {
                    eprintln!("velt dev: {said}");
                }
            }
        }
        carried.own = Some(own);
        carried.interfaces = interfaces;
        // The previous version's kept check is dropped first, so the memory said is this one's.
        carried.kept = None;
        carried.kept = sess.kept.take();
        if let (true, Some(kept)) = (verbose, &carried.kept) {
            eprintln!("velt dev: {}", kept_report(kept));
        }
    }
    let mut files: Vec<_> = sess.sm.files().map(|(_, f)| f.path.clone()).collect();
    files.extend(source_files(opts.packages.as_ref()));
    let program = program.and_then(|p| {
        load(&sess).map_err(crate::driver::BuildError::Failed)?;
        Ok(p)
    });
    let program = match program {
        Ok(program) => program,
        Err(err) => {
            carried.seed = Some(running);
            report(&sess, verbose);
            failure_code(&err);
            return Reloaded::Failed { files };
        }
    };
    carried.seed = next;
    if let Some(reason) = changed {
        report(&sess, verbose);
        return Reloaded::Restart { reason, files };
    }
    let start = Instant::now();
    let mut steps = vec![];
    let outcome = session.reload_timed(&program, &mut steps);
    sess.timings.push(("jit", start.elapsed()));
    sess.record_details("jit", &steps);
    report(&sess, verbose);
    match outcome {
        Ok(Reload::Swapped { functions }) => Reloaded::Swapped { functions, files },
        Ok(Reload::Restart(reason)) => Reloaded::Restart { reason, files },
        Err(e) => Reloaded::Restart {
            reason: format!("code generation failed: {e}"),
            files,
        },
    }
}

#[cfg(test)]
mod tests;
