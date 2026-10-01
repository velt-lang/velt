//! `velt fmt [<file|dir>...] [--check]` argument parsing.

use std::ffi::OsString;
use std::path::PathBuf;

use super::Command;

/// Parse the arguments of `velt fmt`.
pub(super) fn parse_fmt(args: Vec<OsString>) -> Result<Command, String> {
    let mut paths = vec![];
    let mut check = false;
    for arg in args {
        match arg.to_str() {
            Some("--check") => check = true,
            Some(s) if s.starts_with('-') && s.len() > 1 => {
                return Err(super::unknown_option("fmt", s))
            }
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    Ok(Command::Fmt { paths, check })
}

#[cfg(test)]
mod tests {
    use super::super::tests::p;
    use super::*;

    #[test]
    fn fmt_args() {
        assert_eq!(
            p(&["fmt"]).unwrap(),
            Command::Fmt {
                paths: vec![],
                check: false
            }
        );
        assert_eq!(
            p(&["fmt", "a.vlt", "--check", "src"]).unwrap(),
            Command::Fmt {
                paths: vec![PathBuf::from("a.vlt"), PathBuf::from("src")],
                check: true
            }
        );
        assert!(p(&["fmt", "--write"])
            .unwrap_err()
            .contains("unknown option"));
    }
}
