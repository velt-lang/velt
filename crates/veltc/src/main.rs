//! `velt` CLI entry point: parse arguments, dispatch to [`veltc::commands`].
//! See docs/internals/contracts/cli.md.

use std::process::ExitCode;

use veltc::cli;

fn main() -> ExitCode {
    match cli::parse(std::env::args_os().skip(1)) {
        Ok(cmd) => veltc::commands::execute(cmd),
        Err(e) => {
            veltc::style::error(&e);
            ExitCode::from(2)
        }
    }
}
