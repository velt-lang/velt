//! Prints the path of the pinned zig, downloaded and checked against its SHA-256 into
//! `<home>/cache/zig/` if it isn't there yet. CI uses it to set `VELT_ZIG`
//! (.github/actions/zig): `cargo run -p velt_toolchain --example zig -- <home>`.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(home) = std::env::args_os().nth(1) else {
        eprintln!("usage: zig <home>");
        return ExitCode::FAILURE;
    };
    match velt_toolchain::zig::zig(Path::new(&home)) {
        Ok(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
