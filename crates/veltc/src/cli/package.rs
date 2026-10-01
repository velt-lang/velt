//! Argument parsing for the package-manager and test subcommands (`new`, `init`, `add`,
//! `install`, `update`, `publish`, `test`).

use std::ffi::OsString;
use std::path::PathBuf;

use super::build::take_value;
use super::{strings, Command};
use crate::templates::Template;

/// Parse subcommand `name` with its arguments `rest`.
pub(super) fn parse(name: &str, rest: Vec<OsString>) -> Result<Command, String> {
    let args = strings(rest)?;
    let mut flags = Flags::default();
    let mut positional = vec![];
    let mut it = args.into_iter().map(OsString::from);
    while let Some(arg) = it.next() {
        let s = arg.to_string_lossy().into_owned();
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (s.clone(), None),
        };
        match (name, flag.as_str()) {
            ("new", "--lib") => flags.template = Some(Template::Lib),
            ("new" | "init", "--template") => {
                let t = take_value(inline.as_deref(), &mut it, "--template")?;
                flags.template = Some(Template::parse(&t)?);
            }
            ("init", "--name") => {
                flags.name = Some(take_value(inline.as_deref(), &mut it, "--name")?)
            }
            ("init", "--force") => flags.force = true,
            ("add", "--path") => {
                flags.path = Some(take_value(inline.as_deref(), &mut it, "--path")?)
            }
            ("install" | "test", "--locked") => flags.locked = true,
            ("test", "--release") => flags.release = true,
            ("test", "--watch") => flags.watch = true,
            _ if s.starts_with('-') && s.len() > 1 => return Err(super::unknown_option(name, &s)),
            _ => positional.push(s),
        }
    }
    build_command(name, positional, flags)
}

#[derive(Default)]
struct Flags {
    template: Option<Template>,
    name: Option<String>,
    force: bool,
    locked: bool,
    release: bool,
    watch: bool,
    path: Option<String>,
}

fn build_command(name: &str, positional: Vec<String>, flags: Flags) -> Result<Command, String> {
    let max = if matches!(name, "init" | "install" | "update" | "publish") {
        0
    } else {
        1
    };
    if positional.len() > max {
        return Err(format!(
            "unexpected argument `{}` for `velt {name}`",
            positional[max]
        ));
    }
    let arg = positional.into_iter().next();
    match name {
        "new" => Ok(Command::New {
            name: arg.ok_or(
                "missing package name (e.g. `velt new app`; `velt init` makes the current directory a package)",
            )?,
            template: flags.template.unwrap_or_default(),
        }),
        "init" => Ok(Command::Init {
            name: flags.name,
            template: flags.template.unwrap_or_default(),
            force: flags.force,
        }),
        "add" => {
            let spec = arg.ok_or(
                "missing package (e.g. `velt add json@1.2` or `velt add util --path ../util`)",
            )?;
            let (pkg, version) = match spec.split_once('@') {
                Some((p, v)) if !v.is_empty() => (p.to_string(), Some(v.to_string())),
                Some(_) => {
                    return Err(format!("missing version requirement after `@` in `{spec}`"))
                }
                None => (spec, None),
            };
            Ok(Command::Add {
                name: pkg,
                version,
                path: flags.path,
            })
        }
        "install" => Ok(Command::Install {
            locked: flags.locked,
        }),
        "update" => Ok(Command::Update),
        "publish" => Ok(Command::Publish),
        "test" => Ok(Command::Test {
            path: arg.map(PathBuf::from),
            release: flags.release,
            locked: flags.locked,
            watch: flags.watch,
        }),
        _ => unreachable!("ICE: `{name}` is not a package subcommand"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::p;
    use super::*;

    #[test]
    fn package_commands() {
        assert_eq!(
            p(&["new", "app"]).unwrap(),
            Command::New {
                name: "app".into(),
                template: Template::App
            }
        );
        assert_eq!(
            p(&["new", "--lib", "x"]).unwrap(),
            Command::New {
                name: "x".into(),
                template: Template::Lib
            }
        );
        assert_eq!(
            p(&["new", "x", "--template=websocket"]).unwrap(),
            Command::New {
                name: "x".into(),
                template: Template::Websocket
            }
        );
        assert_eq!(
            p(&["init", "--template", "cli", "--name", "tool", "--force"]).unwrap(),
            Command::Init {
                name: Some("tool".into()),
                template: Template::Cli,
                force: true
            }
        );
        assert_eq!(
            p(&["init"]).unwrap(),
            Command::Init {
                name: None,
                template: Template::App,
                force: false
            }
        );
        assert_eq!(
            p(&["add", "json@^1.2"]).unwrap(),
            Command::Add {
                name: "json".into(),
                version: Some("^1.2".into()),
                path: None
            }
        );
        assert_eq!(
            p(&["add", "util", "--path", "../util"]).unwrap(),
            Command::Add {
                name: "util".into(),
                version: None,
                path: Some("../util".into())
            }
        );
        assert_eq!(
            p(&["add", "util", "--path=../u"]).unwrap(),
            Command::Add {
                name: "util".into(),
                version: None,
                path: Some("../u".into())
            }
        );
        assert_eq!(
            p(&["install", "--locked"]).unwrap(),
            Command::Install { locked: true }
        );
        assert_eq!(p(&["update"]).unwrap(), Command::Update);
        assert_eq!(p(&["publish"]).unwrap(), Command::Publish);
        assert_eq!(
            p(&["test"]).unwrap(),
            Command::Test {
                path: None,
                release: false,
                locked: false,
                watch: false
            }
        );
        assert_eq!(
            p(&["test", "tests", "--release", "--watch"]).unwrap(),
            Command::Test {
                path: Some(PathBuf::from("tests")),
                release: true,
                locked: false,
                watch: true
            }
        );
    }

    #[test]
    fn package_errors() {
        assert!(p(&["new"]).unwrap_err().contains("missing package name"));
        assert!(p(&["add"]).unwrap_err().contains("missing package"));
        assert!(p(&["add", "json@"]).unwrap_err().contains("after `@`"));
        assert!(p(&["add", "x", "--path"])
            .unwrap_err()
            .contains("expects a value"));
        assert!(p(&["install", "x"])
            .unwrap_err()
            .contains("unexpected argument"));
        assert!(p(&["new", "a", "--path", "x"])
            .unwrap_err()
            .contains("unknown option"));
        assert!(p(&["new", "a", "--template", "web"])
            .unwrap_err()
            .contains("unknown template `web`"));
        assert!(p(&["init", "name"])
            .unwrap_err()
            .contains("unexpected argument"));
        assert!(p(&["new", "a", "--force"])
            .unwrap_err()
            .contains("unknown option"));
    }
}
