//! `velt dev`'s warm front end (docs/internals/design/dev-warm-front-end.md step 3): a reload of
//! an edit of function bodies alone, checked from the running version's kept check.

use std::path::{Path, PathBuf};
use std::time::Instant;

use velt_sema::hir;
use velt_vir::vir;

use super::{
    load_stage, lower_checked, on_pipeline_thread, BuildError, BuildOptions, CheckScope, Session,
    VirFor,
};

/// A `velt dev` reload of an edit of function bodies alone (`edited`, named as sema names them),
/// from the running version's kept check (`sess.kept`, which stays there) and the source files of
/// the build that made it (`cold_files`) (dev-warm-front-end.md step 3): the program loaded and
/// parsed again, its sema from `kept` (`KeptCheck::reload`), then lowered as usual. `Ok(None)`
/// when the edit needs the cold front end; `sess.warm_refused` says why.
pub fn compile_for_jit_warm(
    sess: &mut Session,
    opts: &BuildOptions,
    cold_files: &velt_common::SourceMap,
    edited: impl FnOnce(&Session) -> Result<Vec<(PathBuf, String)>, String> + Send,
) -> Result<Option<vir::Program>, BuildError> {
    let Some(kept) = sess.kept.take() else {
        sess.warm_refused = Some("no kept check".into());
        return Ok(None);
    };
    // The kept check moves to the pipeline thread and back (it is not `Sync`). A panic in the
    // warm path (an internal error) takes the cold front end instead, without the kept check,
    // which the panic may have left half-changed: the cold build keeps a new one.
    let s = &mut *sess;
    let (out, kept) = on_pipeline_thread(move || {
        let run = std::panic::AssertUnwindSafe(|| warm_inner(s, opts, &kept, cold_files, edited));
        match std::panic::catch_unwind(run) {
            Ok(out) => Ok((out, Some(kept))),
            Err(panic) => {
                let why = (panic.downcast_ref::<String>().map(String::as_str))
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("a panic");
                s.warm_refused = Some(format!("the warm front end failed: {why}"));
                Ok((Ok(None), None))
            }
        }
    })?;
    sess.kept = kept;
    out
}

fn warm_inner(
    sess: &mut Session,
    opts: &BuildOptions,
    kept: &velt_sema::KeptCheck,
    cold_files: &velt_common::SourceMap,
    edited: impl FnOnce(&Session) -> Result<Vec<(PathBuf, String)>, String>,
) -> Result<Option<vir::Program>, BuildError> {
    {
        let scope = CheckScope {
            require_main: true,
            extra_roots: vec![],
            eager_std: false,
        };
        let loaded = load_stage(sess, opts, &scope)?;
        // The modules the cold check loaded after the program's load (the std modules it
        // demanded, the generated ones) are the kept check's: their files go into this session
        // as they were, under the same ids, which the kept check's spans name.
        let (paths, files) = (kept.module_paths(), kept.module_files());
        let same_modules = loaded.modules.len() <= paths.len()
            && (loaded.modules.iter().zip(paths.iter().zip(&files)))
                .all(|(m, (p, (f, _)))| m.path == *p && m.file == *f);
        let loaded_files = sess.sm.files().count();
        let same_files = loaded_files <= cold_files.files().count()
            && (sess.sm.files().zip(cold_files.files())).all(|((_, a), (_, b))| a.path == b.path);
        if !same_modules || !same_files {
            sess.warm_refused = Some("the program loads other modules".into());
            return Ok(None);
        }
        for (_, f) in cold_files.files().skip(loaded_files) {
            sess.sm.add(f.path.clone(), f.src.clone());
        }
        sess.std_files = (files.iter())
            .filter(|(_, std)| *std)
            .map(|(f, _)| *f)
            .collect();
        let t = Instant::now();
        // The edited functions, each named by its module's path.
        let edited = match edited(sess) {
            Ok(files) => files,
            Err(why) => {
                sess.warm_refused = Some(why);
                return Ok(None);
            }
        };
        let module_of = |file: &Path| {
            let file = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
            (loaded.modules.iter()).find(|m| {
                let p = &sess.sm.get(m.file).path;
                std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == file
            })
        };
        let mut named = vec![];
        let mut edited_files = vec![];
        for (file, name) in edited {
            let Some(m) = module_of(&file) else {
                sess.warm_refused = Some(format!("{} is not a module", file.display()));
                return Ok(None);
            };
            named.push((m.path.clone(), name));
            if !edited_files.contains(&m.file) {
                edited_files.push(m.file);
            }
        }
        let edited = named;
        let sema_opts = velt_sema::CheckOptions {
            require_main: scope.require_main,
            eager_std: scope.eager_std,
        };
        let checked = match kept.reload(&loaded.modules, loaded.root, &edited, sema_opts) {
            Ok((p, diags)) => {
                // What the cold front end would report: the warnings of the code the edit left
                // alone, printed again on every reload.
                sess.diagnostics.extend(diags);
                *p
            }
            Err(why) => {
                sess.warm_refused = Some(why);
                return Ok(None);
            }
        };
        sess.record("sema (warm)", t);
        sess.splice_files = Some(edited_files);
        sess.keep_lowering = true;
        lower_checked(sess, opts, VirFor::Jit, checked).map(Some)
    }
}

/// A copy of a build's source files, under the same ids (`compile_for_jit_warm`'s `cold_files`).
pub fn copy_files(sm: &velt_common::SourceMap) -> velt_common::SourceMap {
    let mut copy = velt_common::SourceMap::new();
    for (_, f) in sm.files() {
        copy.add(f.path.clone(), f.src.clone());
    }
    copy
}

/// What a lowering for `velt dev` ([`lower_kept`]) leaves to the optimizer.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum DevOpt {
    /// The whole program, for the debug pipeline to optimize and keep
    /// (`velt_opt::optimize_debug_kept`, `Session::debug_kept`).
    Whole,
    /// Nothing: the last version's program with the spliced functions (these) optimized.
    Done(Vec<usize>),
}

/// Lowering for `velt dev` (dev-warm-front-end.md step 3 "E"): a warm reload's program into the
/// kept lowering (`Session::splice_files`), applied to the last version's optimized program
/// (`Session::dev_program`), whose changed functions alone are optimized; or a program lowered
/// and kept (`Session::keep_lowering`, seeded builds only), which then holds it. `None`: lower
/// as usual, `hir` left in place.
pub(super) fn lower_kept(
    sess: &mut Session,
    hir: &mut Option<Box<hir::Program>>,
    lower_opts: &velt_vir::LowerOptions,
    seeded: bool,
) -> Result<Option<(vir::Program, DevOpt)>, Vec<velt_common::Diagnostic>> {
    let ready = sess.dev_program.is_some() && sess.debug_kept.is_some();
    if let Some((files, kept)) = (sess.splice_files.take()).zip(sess.kept_lowering.take()) {
        if let Some(program) = hir.take_if(|_| ready) {
            match kept.splice(program, &files, lower_opts) {
                Ok((delta, kept)) => match apply(sess, delta) {
                    Ok((p, changed)) => {
                        sess.counted_seed = Some(kept.seed().clone());
                        sess.kept_lowering = Some(kept);
                        return Ok(Some((p, DevOpt::Done(changed))));
                    }
                    // The spliced program lowered whole instead.
                    Err(why) => {
                        sess.splice_refused = Some(why);
                        *hir = Some(kept.into_hir());
                    }
                },
                Err((why, program)) => {
                    sess.splice_refused = Some(why);
                    *hir = Some(program);
                }
            }
        }
    }
    sess.dev_program = None;
    sess.debug_kept = None;
    if !sess.keep_lowering || !seeded {
        return Ok(None);
    }
    let Some(program) = hir.take() else {
        return Ok(None);
    };
    let (p, seed, kept) = velt_vir::KeptLowering::lower(program, lower_opts)?;
    sess.counted_seed = Some(seed);
    sess.kept_lowering = Some(kept);
    Ok(Some((p, DevOpt::Whole)))
}

/// `delta` applied to the last version's program, its changed functions optimized
/// (`velt_opt::optimize_debug_delta`); why not when the optimizer refuses.
fn apply(sess: &mut Session, delta: velt_vir::Delta) -> Result<(vir::Program, Vec<usize>), String> {
    let (Some(mut program), Some(mut kept)) = (sess.dev_program.take(), sess.debug_kept.take())
    else {
        return Err("no program kept".into());
    };
    let changed = delta.apply(&mut program);
    let t = Instant::now();
    let mut passes = velt_opt::PassTimings::new();
    velt_opt::optimize_debug_delta(&mut program, &mut kept, &changed, &mut passes)?;
    sess.record("optimize", t);
    sess.record_details("optimize", passes.entries());
    sess.debug_kept = Some(kept);
    Ok((program, changed))
}

/// The debug pipeline over a program lowered and kept for `velt dev`, keeping what it learns
/// (`Session::debug_kept`), then verification.
pub(super) fn optimize_kept(sess: &mut Session, program: &mut vir::Program) -> Result<(), BuildError> {
    let t = Instant::now();
    let mut passes = velt_opt::PassTimings::new();
    sess.debug_kept = Some(velt_opt::optimize_debug_kept(program, &mut passes));
    let verify_start = Instant::now();
    let verified = velt_vir::verify(program);
    let mut steps = passes.entries().to_vec();
    steps.push(("verify", verify_start.elapsed()));
    sess.record("optimize", t);
    sess.record_details("optimize", &steps);
    verified.map_err(|errs| {
        BuildError::Ice(format!(
            "VIR verification failed after optimization:\n  {}",
            errs.join("\n  ")
        ))
    })
}
