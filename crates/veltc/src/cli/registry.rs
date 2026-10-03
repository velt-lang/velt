//! Argument parsing for the registry-management commands: `yank`, `owner`, `search` and
//! `registry user` (`registry serve` is in [`super::tools`]).

use std::ffi::OsString;
use std::path::PathBuf;

use super::build::take_value;
use super::{strings, Command};

/// What `velt owner` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerAction {
    /// `velt owner list <pkg>`.
    List,
    /// `velt owner add <pkg> <user>`.
    Add(String),
    /// `velt owner remove <pkg> <user>`.
    Remove(String),
}

/// What `velt registry user` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAction {
    /// Create a user and print its token.
    Add,
    /// Delete a user; deleting the last one opens the registry and needs `open` (`--open`).
    Remove {
        /// `--open`.
        open: bool,
    },
    /// Replace a user's token and print the new one.
    Token,
}

/// `velt yank <pkg>@<version> [--undo]`.
pub(super) fn parse_yank(args: Vec<OsString>) -> Result<Command, String> {
    let mut undo = false;
    let mut spec = None;
    for arg in strings(args)? {
        match arg.as_str() {
            "--undo" => undo = true,
            a if a.starts_with('-') => return Err(super::unknown_option("yank", a)),
            _ if spec.is_some() => {
                return Err(format!("unexpected argument `{arg}` for `velt yank`"))
            }
            _ => spec = Some(arg),
        }
    }
    let spec = spec.ok_or("missing package version (e.g. `velt yank json@1.2.0`)")?;
    match spec.split_once('@') {
        Some((name, version)) if !name.is_empty() && !version.is_empty() => Ok(Command::Yank {
            name: name.to_string(),
            version: version.to_string(),
            undo,
        }),
        _ => Err(format!(
            "`{spec}` is not `<package>@<version>` (e.g. `velt yank json@1.2.0`)"
        )),
    }
}

/// `velt owner list|add|remove <pkg> [<user>]`.
pub(super) fn parse_owner(args: Vec<OsString>) -> Result<Command, String> {
    let args = strings(args)?;
    if let Some(flag) = args.iter().find(|a| a.starts_with('-')) {
        return Err(super::unknown_option("owner", flag));
    }
    let usage = "usage: velt owner list <pkg> | velt owner add <pkg> <user> | velt owner remove <pkg> <user>";
    let (action, package) = match args.as_slice() {
        [cmd, pkg] if cmd == "list" => (OwnerAction::List, pkg),
        [cmd, pkg, user] if cmd == "add" => (OwnerAction::Add(user.clone()), pkg),
        [cmd, pkg, user] if cmd == "remove" => (OwnerAction::Remove(user.clone()), pkg),
        _ => return Err(usage.into()),
    };
    Ok(Command::Owner {
        action,
        package: package.clone(),
    })
}

/// `velt search <text> [--json]`.
pub(super) fn parse_search(args: Vec<OsString>) -> Result<Command, String> {
    let mut args = strings(args)?;
    let json = args.iter().any(|a| a == "--json");
    args.retain(|a| a != "--json");
    if let Some(flag) = args.iter().find(|a| a.starts_with('-')) {
        return Err(super::unknown_option("search", flag));
    }
    if args.is_empty() {
        return Err("missing search text (e.g. `velt search json`)".into());
    }
    Ok(Command::Search {
        query: args.join(" "),
        json,
    })
}

/// `velt login <registry-url>` / `velt logout <registry-url>`.
pub(super) fn parse_login(sub: &str, args: Vec<OsString>) -> Result<Command, String> {
    let args = strings(args)?;
    if let Some(flag) = args.iter().find(|a| a.starts_with('-')) {
        return Err(super::unknown_option(sub, flag));
    }
    let [url] = args.as_slice() else {
        return Err(format!(
            "usage: velt {sub} <registry-url> (e.g. `velt {sub} https://registry.example.com`)"
        ));
    };
    let url = url.clone();
    Ok(if sub == "login" {
        Command::Login { url }
    } else {
        Command::Logout { url }
    })
}

/// The words, `--dir` and `--open` of a `velt registry user|owner …` command.
fn admin_args(args: Vec<OsString>) -> Result<(Vec<String>, Option<PathBuf>, bool), String> {
    let (mut words, mut dir, mut open) = (vec![], None, false);
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let arg = arg.to_string_lossy().into_owned();
        match arg.split_once('=') {
            Some(("--dir", v)) => dir = Some(PathBuf::from(v)),
            _ if arg == "--dir" => dir = Some(PathBuf::from(take_value(None, &mut it, "--dir")?)),
            _ if arg == "--open" => open = true,
            _ if arg.starts_with('-') => return Err(super::unknown_option("registry", &arg)),
            _ => words.push(arg),
        }
    }
    Ok((words, dir, open))
}

/// `velt registry user add|remove|token <name> [--dir <d>] [--open]` (after `registry user`).
pub(super) fn parse_user(args: Vec<OsString>) -> Result<Command, String> {
    let usage = "usage: velt registry user add|remove|token <name> [--dir <d>] (remove: [--open])";
    let (words, dir, open) = admin_args(args)?;
    let action = match words.first().map(String::as_str) {
        Some("add") => UserAction::Add,
        Some("remove") => UserAction::Remove { open },
        Some("token") => UserAction::Token,
        _ => return Err(usage.into()),
    };
    if open && action != (UserAction::Remove { open }) {
        return Err(format!("`--open` only goes with `remove` ({usage})"));
    }
    match words.as_slice() {
        [_, name] => Ok(Command::RegistryUser {
            action,
            name: name.clone(),
            dir,
        }),
        _ => Err(usage.into()),
    }
}

/// `velt registry owner add|remove <pkg> <user> [--dir <d>]` (after `registry owner`).
pub(super) fn parse_admin_owner(args: Vec<OsString>) -> Result<Command, String> {
    let usage = "usage: velt registry owner add|remove <pkg> <user> [--dir <d>]";
    let (words, dir, open) = admin_args(args)?;
    if open {
        return Err(format!("unknown option `--open` ({usage})"));
    }
    match words.as_slice() {
        [cmd, package, user] if cmd == "add" || cmd == "remove" => Ok(Command::RegistryOwner {
            add: cmd == "add",
            package: package.clone(),
            user: user.clone(),
            dir,
        }),
        _ => Err(usage.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::p;
    use super::*;

    #[test]
    fn yank_owner_search() {
        assert_eq!(
            p(&["yank", "json@1.2.0"]).unwrap(),
            Command::Yank {
                name: "json".into(),
                version: "1.2.0".into(),
                undo: false
            }
        );
        assert_eq!(
            p(&["yank", "--undo", "json@1.2.0"]).unwrap(),
            Command::Yank {
                name: "json".into(),
                version: "1.2.0".into(),
                undo: true
            }
        );
        assert!(p(&["yank", "json"])
            .unwrap_err()
            .contains("<package>@<version>"));
        assert!(p(&["yank"]).is_err());
        assert_eq!(
            p(&["owner", "add", "json", "bob"]).unwrap(),
            Command::Owner {
                action: OwnerAction::Add("bob".into()),
                package: "json".into()
            }
        );
        assert_eq!(
            p(&["owner", "list", "json"]).unwrap(),
            Command::Owner {
                action: OwnerAction::List,
                package: "json".into()
            }
        );
        assert!(p(&["owner", "add", "json"]).unwrap_err().contains("usage"));
        assert_eq!(
            p(&["search", "json", "schema"]).unwrap(),
            Command::Search {
                query: "json schema".into(),
                json: false
            }
        );
        assert_eq!(
            p(&["search", "--json", "json"]).unwrap(),
            Command::Search {
                query: "json".into(),
                json: true
            }
        );
        assert!(p(&["search"]).is_err());
    }

    #[test]
    fn registry_users() {
        assert_eq!(
            p(&["registry", "user", "add", "alice", "--dir", "r"]).unwrap(),
            Command::RegistryUser {
                action: UserAction::Add,
                name: "alice".into(),
                dir: Some(PathBuf::from("r"))
            }
        );
        assert_eq!(
            p(&["registry", "user", "token", "alice"]).unwrap(),
            Command::RegistryUser {
                action: UserAction::Token,
                name: "alice".into(),
                dir: None
            }
        );
        assert_eq!(
            p(&["registry", "user", "remove", "alice", "--open"]).unwrap(),
            Command::RegistryUser {
                action: UserAction::Remove { open: true },
                name: "alice".into(),
                dir: None
            }
        );
        assert!(p(&["registry", "user", "add", "a", "--open"]).is_err());
        assert_eq!(
            p(&["registry", "owner", "add", "json", "bob", "--dir=r"]).unwrap(),
            Command::RegistryOwner {
                add: true,
                package: "json".into(),
                user: "bob".into(),
                dir: Some(PathBuf::from("r"))
            }
        );
        assert!(p(&["registry", "owner", "list", "json"]).is_err());
        assert!(p(&["registry", "user", "rename", "a"]).is_err());
        assert!(p(&["registry", "user", "add"]).is_err());
    }
}
