//! `velt dev --host`: compile the program to VIR, JIT-compile it with Cranelift and run it in
//! this process with the runtime linked into `velt` (`velt_rt_host`). The supervisor starts one
//! host per version and stops it before the next; a crash or `process.exit` ends only the host.
//!
//! Under the supervisor (`VELT_DEV_SOCKET` set) the host reports its build (ok or failed, plus
//! the files it read) and, after a good build, waits for `go`: the previous version keeps
//! serving until the new one is ready to start. It then keeps the connection as its reload
//! channel and takes later versions in place (`swap`), while the program runs.

use std::process::ExitCode;
use std::time::Instant;

use velt_codegen_cl::{DevSession, JitProgram};

use crate::cli::DevArgs;
use crate::commands::{build_options, failure_code, report};
use crate::driver::{self, BuildError, BuildOptions, Session};

/// Run the program; exits the process with the program's exit code.
pub fn host_command(args: &DevArgs) -> ExitCode {
    let opts = match build_options(&args.build) {
        Ok(opts) => opts,
        Err(msg) => {
            crate::style::error(&msg);
            return ExitCode::from(1);
        }
    };
    let mut sess = Session::new();
    let mut session = DevSession::new(&velt_rt_host::abi_symbols::symbol_table());
    let loaded = compile_and_load(&mut sess, &opts, &mut session);
    report(&sess, args.build.verbose);
    if let Err(ref err) = loaded {
        failure_code(err);
    }
    if let Some(socket) = std::env::var_os(velt_rt_host::dev::SOCKET_ENV) {
        let files: Vec<_> = sess.sm.files().map(|(_, f)| f.path.clone()).collect();
        let ready =
            velt_rt_host::dev::handover::report_build(socket.as_ref(), loaded.is_ok(), &files);
        match ready {
            Ok(Some(channel)) => {
                super::swap::serve_reloads(channel, opts.clone(), session, args.build.verbose)
            }
            Ok(None) => {}
            Err(e) => {
                crate::style::error(&format!("velt dev: {e}"));
                return ExitCode::from(1);
            }
        }
    }
    let Ok(loaded) = loaded else {
        return ExitCode::from(1);
    };
    let mut argv = vec![opts.input.display().to_string()];
    argv.extend(args.args.iter().map(|a| a.to_string_lossy().into_owned()));
    velt_rt_host::process::set_args(argv);
    let code = velt_rt_host::entry::run_main(loaded.main());
    std::process::exit(code)
}

/// Front end, then JIT; the JIT time is recorded as the `jit` stage.
fn compile_and_load(
    sess: &mut Session,
    opts: &BuildOptions,
    session: &mut DevSession,
) -> Result<JitProgram, BuildError> {
    let program = driver::compile(sess, opts)?;
    let start = Instant::now();
    let loaded = session
        .load(&program)
        .map_err(|e| BuildError::Failed(format!("code generation failed: {e}")));
    sess.timings.push(("jit", start.elapsed()));
    loaded
}
