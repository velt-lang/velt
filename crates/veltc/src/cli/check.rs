//! `velt check` argument parsing.

use std::ffi::OsString;
use std::path::PathBuf;

use super::Command;

/// `velt check` arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckArgs {
    /// Root file; `None` → the package around the current directory.
    pub input: Option<PathBuf>,
    /// `--ts-compat [<file|dir>...]`: check these files, then lint them for the TypeScript/Velt
    /// common subset; empty: the `tsCompat` folders of the package around the current directory.
    pub ts_compat: Option<Vec<PathBuf>>,
    /// `--json`: diagnostics as one JSON document on stdout instead of text on stderr.
    pub json: bool,
    /// `--locked`.
    pub locked: bool,
    /// `-v`: per-stage timings on stderr.
    pub verbose: bool,
    /// `--std`: check every file of the standard library, every definition, reached or not
    /// (for the standard library's own development).
    pub std: bool,
    /// `--target <triple>`: check for that target (the `#name` variants it selects, the system
    /// functions it has); `None` → the host.
    pub target: Option<String>,
}

/// Parse `velt check [<file.vlt>] [--target <triple>] [--json] [--locked] [-v]`, `velt check
/// --std` and `velt check --ts-compat [<file|dir>...] [--json] [--locked] [-v]`.
pub(super) fn parse_check(args: Vec<OsString>) -> Result<Command, String> {
    let mut c = CheckArgs::default();
    let mut ts_compat = false;
    let mut paths = vec![];
    let mut it = super::strings(args)?.into_iter();
    while let Some(arg) = it.next() {
        if let Some(triple) = arg.strip_prefix("--target=") {
            c.target = Some(triple.to_string());
            continue;
        }
        match arg.as_str() {
            "--target" => {
                let triple = it.next().ok_or("`--target` expects a value")?;
                c.target = Some(triple);
            }
            "--json" => c.json = true,
            "--locked" => c.locked = true,
            "--std" => c.std = true,
            "--ts-compat" => ts_compat = true,
            "-v" | "--verbose" => c.verbose = true,
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(super::unknown_option("check", s))
            }
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    if ts_compat {
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
            p(&["check", "--std"]).unwrap(),
            Command::Check(CheckArgs {
                std: true,
                ..CheckArgs::default()
            })
        );
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
        for args in [
            &["check", "--target", "wasm32-unknown-unknown"][..],
            &["check", "--target=wasm32-unknown-unknown"],
        ] {
            assert_eq!(
                p(args).unwrap(),
                Command::Check(CheckArgs {
                    target: Some("wasm32-unknown-unknown".into()),
                    ..CheckArgs::default()
                })
            );
        }
        assert!(p(&["check", "--target"])
            .unwrap_err()
            .contains("expects a value"));
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
        // No paths: the package's `tsCompat` folders.
        assert_eq!(
            p(&["check", "--ts-compat"]).unwrap(),
            Command::Check(CheckArgs {
                ts_compat: Some(vec![]),
                ..CheckArgs::default()
            })
        );
    }
}
