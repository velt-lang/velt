//! The `velt` on PATH (#948, docs/tooling/platforms.md "Toolchain versions").
//!
//! Installed as `<root>/bin/velt`, it picks a toolchain ([`select`]): a first argument
//! `+<toolchain>` (`velt +0.2 build`), else `$VELT_TOOLCHAIN`, else the `velt` field of the
//! nearest `package.vlt`, else `<root>/default`. It installs a missing
//! version from the release (unless `VELT_TOOLCHAIN_AUTO_INSTALL=0`) and runs that toolchain's
//! `bin/velt` with the same arguments ([`exec`]), so every command behaves as the toolchain's
//! own. `velt toolchain …` is the launcher's own command ([`commands`]), the same for every
//! version.

pub mod commands;
pub mod exec;
pub mod select;

use std::ffi::OsString;

use velt_toolchain::Root;

/// Environment variables the launcher reads.
pub const ENV_TOOLCHAIN: &str = "VELT_TOOLCHAIN";
pub const ENV_AUTO_INSTALL: &str = "VELT_TOOLCHAIN_AUTO_INSTALL";
/// Set for the toolchain it runs (`velt doctor` shows them).
pub const ENV_LAUNCHER: &str = "VELT_LAUNCHER";
pub const ENV_SELECTED: &str = "VELT_TOOLCHAIN_SELECTED";

/// Run the launcher with `args` (without the program name); returns the exit code.
pub fn main(args: &[OsString]) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            1
        }
    }
}

fn run(args: &[OsString]) -> Result<i32, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot find the launcher's own path: {e}"))?;
    let root = Root::of_launcher(&exe).ok_or_else(|| {
        format!(
            "the velt launcher must be installed as <root>/bin/velt (it is {}); the installer \
             puts it in ~/.velt/bin",
            exe.display()
        )
    })?;
    let mut ctx = select::Context::from_env(root)?;
    let mut args = args;
    if let Some(name) = args
        .first()
        .and_then(|a| a.to_str())
        .and_then(|a| a.strip_prefix('+'))
    {
        ctx.plus = Some(name.to_string());
        args = &args[1..];
    }
    if args.first().is_some_and(|a| a == "toolchain") {
        return commands::run(&ctx, &args[1..]).map(|()| 0);
    }
    let selection = select::select(&ctx)?;
    let resolved = select::resolve(&ctx, &selection)?;
    exec::exec(&resolved, args, &exe)
}
