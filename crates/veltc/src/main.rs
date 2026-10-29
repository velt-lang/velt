//! `velt` CLI entry point: parse arguments, dispatch to [`veltc::commands`].
//! See docs/internals/contracts/cli.md.

use std::process::ExitCode;

use veltc::cli;

fn main() -> ExitCode {
    // The launcher counts launchers running one another (#948); a toolchain ends the chain, so
    // a `velt` that a program it runs starts again counts from zero. Before any thread starts.
    std::env::remove_var("VELT_LAUNCHER_HOPS");
    // `--workspace`, `-p` and `--affected` choose the members a command runs on (#1855).
    let args = match veltc::commands::members::intercept(std::env::args_os().skip(1).collect()) {
        veltc::commands::members::Intercept::Done(code) => return code,
        veltc::commands::members::Intercept::Continue(args) => args,
    };
    match cli::parse(args) {
        Ok(cmd) => veltc::commands::execute(cmd),
        Err(e) => {
            veltc::style::error(&e);
            ExitCode::from(2)
        }
    }
}
