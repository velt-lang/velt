//! `velt dev` argument parsing: `run`'s arguments plus the mode flags.

use std::ffi::OsString;

use super::build::parse_build;
use super::{Command, DevArgs, DevMode};

/// Parse `velt dev [<file>] [--exe] [--locked] [-v] [-- <program args>...]` (and the internal
/// `--host`, which the supervisor passes to the JIT host child).
pub(super) fn parse_dev(args: Vec<OsString>) -> Result<Command, String> {
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let mut mode = DevMode::Jit;
    let mut rest = vec![];
    for (i, arg) in args.into_iter().enumerate() {
        match arg.to_str() {
            Some("--exe") if i < split => mode = DevMode::Exe,
            Some("--host") if i < split => mode = DevMode::Host,
            _ => rest.push(arg),
        }
    }
    let (build, args) = parse_build(rest, "dev")?;
    let optimized = build.release || build.debug_info || build.backend.is_some();
    if optimized && mode != DevMode::Exe {
        return Err(
            "`--release`, `-g` and `--backend` need `velt dev --exe` (the JIT host always builds for development)"
                .into(),
        );
    }
    Ok(Command::Dev(DevArgs { build, args, mode }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::tests::p;
    use super::*;

    fn dev(args: &[&str]) -> DevArgs {
        match p(args).unwrap() {
            Command::Dev(d) => d,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn modes_and_program_args() {
        let d = dev(&["dev", "app.vlt", "--", "--exe", "x"]);
        assert_eq!(d.mode, DevMode::Jit);
        assert_eq!(d.build.input, Some(PathBuf::from("app.vlt")));
        assert_eq!(d.args, ["--exe", "x"].map(OsString::from));
        assert_eq!(dev(&["dev", "--exe"]).mode, DevMode::Exe);
        assert_eq!(dev(&["dev", "--host", "a.vlt"]).mode, DevMode::Host);
        assert!(dev(&["dev", "--exe", "--release"]).build.release);
        assert!(p(&["dev", "--release"]).unwrap_err().contains("--exe"));
        assert!(p(&["dev", "-o", "x"])
            .unwrap_err()
            .contains("unknown option"));
    }

    #[test]
    fn unknown_option_names_dev() {
        let err = p(&["dev", "--exee", "a.vlt"]).unwrap_err();
        assert!(err.contains("for `velt dev`"), "{err}");
        assert!(err.contains("--exe"), "{err}");
    }
}
