//! Argument parsing for the developer tools: `velt playground`, `velt doc`,
//! `velt registry serve`.

use std::ffi::OsString;
use std::path::PathBuf;

use super::build::take_value;
use super::{Command, DocArgs, RegistryArgs};
use crate::playground::PlaygroundArgs;

/// Default playground port.
const PLAYGROUND_PORT: u16 = 8090;
/// Default registry server port.
const REGISTRY_PORT: u16 = 8091;

/// `velt playground [--port <n>] [--host <addr>]`.
pub(super) fn parse_playground(args: Vec<OsString>) -> Result<Command, String> {
    let (addr, dir) = parse_server(args, PLAYGROUND_PORT, false, "playground")?;
    debug_assert!(dir.is_none());
    Ok(Command::Playground(PlaygroundArgs { addr }))
}

/// `velt registry serve [--dir <d>] [--port <n>] [--host <addr>]`.
pub(super) fn parse_registry(args: Vec<OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    match args.next().as_ref().and_then(|a| a.to_str()) {
        Some("serve") => {}
        _ => {
            return Err(
                "usage: velt registry serve [--dir <d>] [--port <n>] [--host <addr>]".into(),
            )
        }
    }
    let (addr, dir) = parse_server(args.collect(), REGISTRY_PORT, true, "registry serve")?;
    Ok(Command::RegistryServe(RegistryArgs { dir, addr }))
}

/// `--host`/`--port` (and `--dir` when `with_dir`) of a server command.
fn parse_server(
    args: Vec<OsString>,
    default_port: u16,
    with_dir: bool,
    cmd: &str,
) -> Result<(String, Option<PathBuf>), String> {
    let (mut host, mut port, mut dir) = ("127.0.0.1".to_string(), default_port, None);
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let arg = arg.to_string_lossy().into_owned();
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        match flag.as_str() {
            "--host" => host = take_value(inline.as_deref(), &mut it, &flag)?,
            "--port" => {
                let v = take_value(inline.as_deref(), &mut it, &flag)?;
                port = v.parse().map_err(|_| format!("invalid port `{v}`"))?;
            }
            "--dir" if with_dir => {
                dir = Some(PathBuf::from(take_value(
                    inline.as_deref(),
                    &mut it,
                    &flag,
                )?))
            }
            _ if arg.starts_with('-') => return Err(super::unknown_option(cmd, &arg)),
            _ => return Err(format!("unexpected argument `{arg}` for `velt {cmd}`")),
        }
    }
    Ok((format!("{host}:{port}"), dir))
}

/// `velt doc [<file|dir>...] [--std] [-o <dir>]`.
pub(super) fn parse_doc(args: Vec<OsString>) -> Result<Command, String> {
    let mut doc = DocArgs::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--std") => doc.std = true,
            Some("-o" | "--output") => {
                doc.output = Some(PathBuf::from(take_value(None, &mut it, "-o")?));
            }
            Some(s) if s.starts_with("--output=") => {
                doc.output = Some(PathBuf::from(&s["--output=".len()..]));
            }
            Some(s) if s.starts_with('-') => {
                return Err(super::unknown_option("doc", s));
            }
            _ => doc.paths.push(PathBuf::from(arg)),
        }
    }
    Ok(Command::Doc(doc))
}

#[cfg(test)]
mod tests {
    use super::super::tests::p;
    use super::*;

    #[test]
    fn playground_options() {
        let addr = |args: &[&str]| match p(args).unwrap() {
            Command::Playground(a) => a.addr,
            other => panic!("{other:?}"),
        };
        assert_eq!(addr(&["playground"]), "127.0.0.1:8090");
        assert_eq!(
            addr(&["playground", "--port=0", "--host", "0.0.0.0"]),
            "0.0.0.0:0"
        );
        assert!(p(&["playground", "--port", "x"]).is_err());
        assert!(p(&["playground", "extra"]).is_err());
    }

    #[test]
    fn registry_options() {
        match p(&["registry", "serve", "--dir", "r", "--port=0"]).unwrap() {
            Command::RegistryServe(a) => {
                assert_eq!(a.dir, Some(PathBuf::from("r")));
                assert_eq!(a.addr, "127.0.0.1:0");
            }
            other => panic!("{other:?}"),
        }
        assert!(p(&["registry"]).is_err());
        assert!(p(&["playground", "--dir", "x"]).is_err());
    }

    #[test]
    fn doc_options() {
        let doc = |args: &[&str]| match p(args).unwrap() {
            Command::Doc(d) => d,
            other => panic!("{other:?}"),
        };
        assert_eq!(doc(&["doc"]), DocArgs::default());
        let d = doc(&["doc", "--std", "-o", "out", "a.vlt"]);
        assert!(d.std);
        assert_eq!(d.output, Some(PathBuf::from("out")));
        assert_eq!(d.paths, [PathBuf::from("a.vlt")]);
        assert!(p(&["doc", "--nope"]).is_err());
    }
}
