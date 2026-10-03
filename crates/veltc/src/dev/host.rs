//! `velt dev --host`: compile the program to VIR, JIT-compile it with Cranelift and run it in
//! this process with the runtime linked into `velt` (`velt_rt_host`). The supervisor starts one
//! host per version and stops it before the next; a crash or `process.exit` ends only the host.
//!
//! Under the supervisor (`VELT_DEV_SOCKET` set) the host reports its build (ok or failed, plus
//! the files it read) and, after a good build, waits for `go`: the previous version keeps
//! serving until the new one is ready to start. It then keeps the connection as its reload
//! channel and takes later versions in place (`swap`), while the program runs. With
//! [`QUIET_ENV`] set it prints nothing about its build. With [`DEBUG_INFO_ENV`] set to `0` it
//! does not describe its code to debuggers.

use std::process::ExitCode;
use std::time::Instant;

use velt_codegen_cl::{DevSession, JitProgram};

use crate::cli::DevArgs;
use crate::commands::{build_options, failure_code, report};
use crate::driver::{self, BuildError, BuildOptions, Session};

/// Set for a host the supervisor starts while the running one is still deciding whether it can
/// take the change: its build output would repeat the running host's.
pub const QUIET_ENV: &str = "VELT_DEV_QUIET";

/// `0` turns off the line tables the host registers for debuggers (GDB JIT interface), which
/// saves building an in-memory debug image for every version.
pub const DEBUG_INFO_ENV: &str = "VELT_DEV_DEBUG_INFO";

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
    sess.show_details = args.build.timings;
    let natives = match super::native::jit_symbols(opts.packages.as_ref()) {
        Ok(natives) => natives,
        Err(msg) => {
            crate::style::error(&msg);
            return ExitCode::from(1);
        }
    };
    let mut symbols = velt_rt_host::abi_symbols::symbol_table();
    symbols.extend(natives.iter().map(|(n, a)| (n.as_str(), *a as *const u8)));
    let mut session = DevSession::new(&symbols);
    session.set_debug_info(std::env::var_os(DEBUG_INFO_ENV).is_none_or(|v| v != "0"));
    let loaded = compile_and_load(&mut sess, &opts, &mut session);
    // A host started ahead of need stays quiet: the running version reports the same build.
    if std::env::var_os(QUIET_ENV).is_none() {
        report(&sess, args.build.verbose);
        if let Err(ref err) = loaded {
            failure_code(err);
        }
    }
    if let Some(socket) = std::env::var_os(velt_rt_host::dev::SOCKET_ENV) {
        let mut files: Vec<_> = sess.sm.files().map(|(_, f)| f.path.clone()).collect();
        files.extend(super::native::source_files(opts.packages.as_ref()));
        let ready =
            velt_rt_host::dev::handover::report_build(socket.as_ref(), loaded.is_ok(), &files);
        match ready {
            Ok(Some(channel)) => {
                super::swap::serve_reloads(channel, opts.clone(), session, &args.build)
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

/// Front end, then JIT; the JIT time is recorded as the `jit` stage, with its steps.
fn compile_and_load(
    sess: &mut Session,
    opts: &BuildOptions,
    session: &mut DevSession,
) -> Result<JitProgram, BuildError> {
    let program = driver::compile(sess, opts)?;
    let start = Instant::now();
    let mut steps = vec![];
    let loaded = session
        .load_timed(&program, &mut steps)
        .map_err(|e| BuildError::Failed(format!("code generation failed: {e}")));
    sess.timings.push(("jit", start.elapsed()));
    sess.record_details("jit", &steps);
    loaded
}
