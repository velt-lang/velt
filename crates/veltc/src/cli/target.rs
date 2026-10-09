//! `velt target list|add|remove`: the targets this toolchain can build for (#856).

use std::ffi::OsString;
use std::path::PathBuf;

use super::{strings, Command};

/// What `velt target` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetAction {
    /// `velt target list`.
    List,
    /// `velt target add <triple>... [--from <file>] [--unverified]`: install target packs,
    /// downloaded from the release of this `velt`, or one read from a file (`--unverified`: one
    /// that cannot be checked, e.g. built locally).
    Add {
        targets: Vec<String>,
        from: Option<PathBuf>,
        unverified: bool,
    },
    /// `velt target remove <triple>...`.
    Remove { targets: Vec<String> },
}

/// `velt target ...`.
pub fn parse_target(rest: Vec<OsString>) -> Result<Command, String> {
    let args = strings(rest)?;
    let Some((action, rest)) = args.split_first() else {
        return Err(
            "missing action: `velt target list`, `add <triple>` or `remove <triple>`".into(),
        );
    };
    let mut targets = vec![];
    let mut from = None;
    let mut unverified = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--from" if action == "add" => {
                let file = it
                    .next()
                    .ok_or("`--from` needs a file (a target pack .tar.gz)")?;
                from = Some(PathBuf::from(file));
            }
            "--unverified" if action == "add" => unverified = true,
            flag if flag.starts_with('-') => {
                return Err(super::unknown_option(&format!("target {action}"), flag))
            }
            triple => targets.push(triple.to_string()),
        }
    }
    let action = match action.as_str() {
        "list" if targets.is_empty() => TargetAction::List,
        "list" => {
            return Err(format!(
                "unexpected argument `{}` for `velt target list`",
                targets[0]
            ))
        }
        "add" | "remove" if targets.is_empty() => {
            return Err(format!(
                "missing target triple for `velt target {action}` (`velt target list` shows them)"
            ))
        }
        "add" if from.is_some() && targets.len() > 1 => {
            return Err("`--from` installs one target pack: give one triple".into())
        }
        "add" if unverified && from.is_none() => {
            return Err(
                "`--unverified` goes with `--from <file>` (downloads are always checked)".into(),
            )
        }
        "add" => TargetAction::Add {
            targets,
            from,
            unverified,
        },
        "remove" => TargetAction::Remove { targets },
        other => {
            return Err(format!(
                "unknown action `{other}` for `velt target` (list, add, remove)"
            ))
        }
    };
    Ok(Command::Target(action))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, String> {
        parse_target(args.iter().map(OsString::from).collect())
    }

    #[test]
    fn actions() {
        assert_eq!(p(&["list"]).unwrap(), Command::Target(TargetAction::List));
        assert_eq!(
            p(&["add", "x86_64-pc-windows-msvc", "aarch64-apple-darwin"]).unwrap(),
            Command::Target(TargetAction::Add {
                targets: vec![
                    "x86_64-pc-windows-msvc".into(),
                    "aarch64-apple-darwin".into()
                ],
                from: None,
                unverified: false,
            })
        );
        assert_eq!(
            p(&[
                "add",
                "x86_64-unknown-linux-musl",
                "--from",
                "pack.tar.gz",
                "--unverified"
            ])
            .unwrap(),
            Command::Target(TargetAction::Add {
                targets: vec!["x86_64-unknown-linux-musl".into()],
                from: Some("pack.tar.gz".into()),
                unverified: true,
            })
        );
        assert_eq!(
            p(&["remove", "x86_64-apple-darwin"]).unwrap(),
            Command::Target(TargetAction::Remove {
                targets: vec!["x86_64-apple-darwin".into()]
            })
        );
    }

    #[test]
    fn mistakes() {
        assert!(p(&[]).unwrap_err().contains("missing action"));
        assert!(p(&["add"]).unwrap_err().contains("missing target"));
        assert!(p(&["list", "x"]).is_err());
        assert!(p(&["add", "a", "b", "--from", "f"])
            .unwrap_err()
            .contains("one triple"));
        assert!(p(&["remove", "x", "--from", "f"]).is_err());
        assert!(p(&["install", "x"]).unwrap_err().contains("unknown action"));
        assert!(p(&["add", "x", "--unverified"])
            .unwrap_err()
            .contains("--from"));
    }
}
