//! `velt check` argument parsing.

use std::ffi::OsString;
use std::path::PathBuf;

use super::Command;

/// `velt check` arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckArgs {
    /// Root file; `None` → the package around the current directory.
    pub input: Option<PathBuf>,
    /// `--json`: diagnostics as one JSON document on stdout instead of text on stderr.
    pub json: bool,
    /// `--locked`.
    pub locked: bool,
    /// `-v`: per-stage timings on stderr.
    pub verbose: bool,
}

/// Parse `velt check [<file.vlt>] [--json] [--locked] [-v]`.
pub(super) fn parse_check(args: Vec<OsString>) -> Result<Command, String> {
    let mut c = CheckArgs::default();
    for arg in super::strings(args)? {
        match arg.as_str() {
            "--json" => c.json = true,
            "--locked" => c.locked = true,
            "-v" | "--verbose" => c.verbose = true,
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(super::unknown_option("check", s))
            }
            _ if c.input.is_some() => {
                return Err(format!("unexpected argument `{arg}` for `velt check`"))
            }
            _ => c.input = Some(PathBuf::from(arg)),
        }
    }
    Ok(Command::Check(c))
}

#[cfg(test)]
mod tests {
    use super::super::tests::p;
    use super::*;

    #[test]
    fn check_args() {
        assert_eq!(p(&["check"]).unwrap(), Command::Check(CheckArgs::default()));
        assert_eq!(
            p(&["check", "a.vlt", "--json", "--locked", "-v"]).unwrap(),
            Command::Check(CheckArgs {
                input: Some(PathBuf::from("a.vlt")),
                json: true,
                locked: true,
                verbose: true,
            })
        );
        assert!(p(&["check", "a.vlt", "b.vlt"])
            .unwrap_err()
            .contains("unexpected argument"));
        assert!(p(&["check", "--jsn"]).unwrap_err().contains("`--json`"));
    }
}
