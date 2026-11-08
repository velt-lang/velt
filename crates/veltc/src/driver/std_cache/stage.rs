//! The std cache around the optimizer: std functions out of the program before it is optimized,
//! their objects linked after (`super`'s module docs).

use std::time::Instant;

use velt_vir::vir;

use super::{no_inline, serve, Use};
use crate::driver::{optimize_stage, outside, BuildError, BuildOptions, OutputPaths, Session};

/// Optimize `program` (lowered and verified, for `target`), using the std cache as `wanted` says.
/// `VELT_STD_OBJECT=1` keeps the program as lowered to fall back on.
pub(in crate::driver) fn optimize(
    sess: &mut Session,
    opts: &BuildOptions,
    target: &str,
    wanted: Use,
    program: &mut vir::Program,
) -> Result<(), BuildError> {
    let cg = velt_codegen_cl::CodegenOptions {
        target: opts.target(),
        optimize: opts.release,
        cpu: opts.cpu.clone(),
    };
    let wanted = match wanted {
        Use::NoInline if !no_inline::applies(program) => Use::Off,
        other => other,
    };
    // A declaration hides what its function reaches. Set either way: a build on this session that
    // stopped early must not leave its answer for this one.
    sess.reaches = (wanted != Use::Off).then(|| outside::reaches(program));
    let place =
        OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, target).std_objects();
    if wanted != Use::Off && sess.turn.is_none() {
        sess.turn = place.lock(sess.verbose);
    }
    let t = Instant::now();
    match wanted {
        Use::Off => optimize_stage(sess, opts, program),
        Use::Debug => {
            sess.std_objects = serve::serve(program, opts, &cg, &place);
            sess.record("std cache", t);
            optimize_stage(sess, opts, program)
        }
        Use::NoInline => {
            let served = no_inline::serve(program, opts, &cg, &place);
            sess.record("std cache", t);
            if served.is_empty() {
                // Nothing hides behind a declaration: what the program reaches is worked out
                // after optimization, as in a build without the cache.
                sess.reaches = None;
            }
            optimize_stage(sess, opts, program)?;
            sess.std_objects = served.linked(program);
            Ok(())
        }
        Use::Release => {
            let unsplit = program.clone();
            let plan = super::split(program, opts, &cg);
            sess.record("std cache", t);
            optimize_stage(sess, opts, program)?;
            let t = Instant::now();
            match super::link(plan, program, &cg, &place) {
                Ok(linked) => {
                    if super::stats() {
                        eprintln!(
                            "velt: std cache: {} found, {} compiled, {} no longer called",
                            linked.hits, linked.misses, linked.dropped
                        );
                    }
                    sess.std_objects = linked.objects;
                }
                Err(why) => {
                    place.clear();
                    if super::stats() {
                        eprintln!("velt: std cache: not used for this build: {why}");
                    }
                    *program = unsplit;
                    optimize_stage(sess, opts, program)?;
                }
            }
            sess.record("std cache link", t);
            Ok(())
        }
    }
}
