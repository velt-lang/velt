//! `velt check` argument parsing.

use std::ffi::OsString;
use std::path::PathBuf;

use super::Command;

/// `velt check` arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckArgs {
    /// Root file; `None` → the package around the current directory.
    pub input: Option<PathBuf>,
    /// `--ts-compat <file|dir>...`: check these files, then lint them for the TypeScript/Velt
    /// common subset (never empty when set).
    pub ts_compat: Option<Vec<PathBuf>>,
    /// `--json`: diagnostics as one JSON document on stdout instead of text on stderr.
    pub json: bool,
    /// `--locked`.
    pub locked: bool,
    /// `-v`: per-stage timings on stderr.
    pub verbose: bool,
}

/// `--ts-compat` without paths (`tsCompat` folders in package.vlt are planned).
const NO_TS_COMPAT_PATHS: &str = "`velt check --ts-compat` needs the files or directories to \
                                  lint, e.g. `velt check --ts-compat src/models`";

/// Parse `velt check [<file.vlt>] [--json] [--locked] [-v]` and
/// `velt check --ts-compat <file|dir>... [--json] [--locked] [-v]`.
pub(super) fn parse_check(args: Vec<OsString>) -> Result<Command, String> {
    let mut c = CheckArgs::default();
    let mut ts_compat = false;
    let mut paths = vec![];
    for arg in super::strings(args)? {
        match arg.as_str() {
            "--json" => c.json = true,
            "--locked" => c.locked = true,
            "--ts-compat" => ts_compat = true,
            "-v" | "--verbose" => c.verbose = true,
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(super::unknown_option("check", s))
            }
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    if ts_compat {
        if paths.is_empty() {
            return Err(NO_TS_COMPAT_PATHS.into());
        }
        c.ts_compat = Some(paths);
    } else if let Some(extra) = paths.get(1) {
        return Err(format!(
            "unexpected argument `{}` for `velt check`",
            extra.display()
        ));
    } else {
        c.input = paths.pop();
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
                ..CheckArgs::default()
            })
        );
        assert!(p(&["check", "a.vlt", "b.vlt"])
            .unwrap_err()
            .contains("unexpected argument `b.vlt`"));
        assert!(p(&["check", "--jsn"]).unwrap_err().contains("`--json`"));
    }

    #[test]
    fn ts_compat_takes_paths() {
        assert_eq!(
            p(&["check", "--ts-compat", "src/models", "a.ts", "--json"]).unwrap(),
            Command::Check(CheckArgs {
                ts_compat: Some(vec![PathBuf::from("src/models"), PathBuf::from("a.ts")]),
                json: true,
                ..CheckArgs::default()
            })
        );
        assert!(p(&["check", "--ts-compat"])
            .unwrap_err()
            .contains("needs the files or directories"));
    }
}
