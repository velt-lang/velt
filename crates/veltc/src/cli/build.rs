//! `velt build` / `velt run` argument parsing.

use std::ffi::OsString;
use std::path::PathBuf;

use super::{BuildArgs, Emit};
use crate::backend::Backend;

/// Parse the build options of `velt <cmd>` (`build`, `run` or `dev`); for the commands that run
/// the program, everything after `--` is returned as program args. `velt run` also passes
/// everything after the file to the program, as `node file.js a b` does: options before the
/// file are Velt's, the ones after it the program's (a `--` right after the file is optional).
pub(super) fn parse_build(
    args: Vec<OsString>,
    cmd: &str,
) -> Result<(BuildArgs, Vec<OsString>), String> {
    let is_run = cmd != "build";
    let mut b = BuildArgs::default();
    let mut prog_args = vec![];
    let mut it = args.into_iter().peekable();
    while let Some(arg) = it.next() {
        let Some(s) = arg.to_str() else {
            set_input(&mut b.input, PathBuf::from(arg))?;
            if cmd == "run" {
                take_program_args(&mut it, &mut prog_args);
                break;
            }
            continue;
        };
        // `--flag=value` form.
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (s, None),
        };
        match flag {
            "--" if is_run => {
                prog_args.extend(it.by_ref());
                break;
            }
            "--release" => b.release = true,
            "-g" => b.debug_info = true,
            "--locked" => b.locked = true,
            "-v" | "--verbose" => b.verbose = true,
            "--timings" => {
                b.verbose = true;
                b.timings = true;
            }
            "-o" | "--output" if !is_run => {
                b.output = Some(PathBuf::from(take_value(inline.as_deref(), &mut it, flag)?))
            }
            "--target" => b.target = Some(take_value(inline.as_deref(), &mut it, flag)?),
            "--backend" => {
                b.backend = Some(Backend::parse(&take_value(
                    inline.as_deref(),
                    &mut it,
                    flag,
                )?)?)
            }
            "--emit" if !is_run => {
                b.emit = parse_emit(&take_value(inline.as_deref(), &mut it, flag)?)?
            }
            _ if s.starts_with('-') && s.len() > 1 => {
                return Err(super::unknown_option(cmd, s));
            }
            _ => {
                set_input(&mut b.input, PathBuf::from(s))?;
                if cmd == "run" {
                    take_program_args(&mut it, &mut prog_args);
                    break;
                }
            }
        }
    }
    Ok((b, prog_args))
}

/// `velt run <file> ...`: the rest belongs to the program, without a leading `--`.
fn take_program_args(
    it: &mut std::iter::Peekable<impl Iterator<Item = OsString>>,
    prog_args: &mut Vec<OsString>,
) {
    if it.peek().is_some_and(|a| a == "--") {
        it.next();
    }
    prog_args.extend(it);
}

/// The arguments of `velt run` that are Velt's: those before the file (or before `--`).
pub(super) fn run_options(args: &[OsString]) -> &[OsString] {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.to_str() {
            Some("--") => break,
            // An option that takes its value from the next argument.
            Some("--target" | "--backend") => i += 2,
            Some(s) if s.starts_with('-') && s.len() > 1 => i += 1,
            _ => break,
        }
    }
    &args[..i.min(args.len())]
}

fn parse_emit(kind: &str) -> Result<Emit, String> {
    match kind {
        "vir" => Ok(Emit::Vir),
        "llvm" => Ok(Emit::Llvm),
        "obj" => Ok(Emit::Obj),
        "exe" => Ok(Emit::Exe),
        other => Err(format!(
            "unknown --emit kind `{other}` (expected vir, llvm, obj or exe)"
        )),
    }
}

/// The value of `name`: inline (`--x=v`) or the next argument.
pub(super) fn take_value(
    inline: Option<&str>,
    it: &mut impl Iterator<Item = OsString>,
    name: &str,
) -> Result<String, String> {
    match inline {
        Some(v) => Ok(v.to_string()),
        None => it
            .next()
            .map(|v| v.to_string_lossy().into_owned())
            .ok_or_else(|| format!("`{name}` expects a value")),
    }
}

fn set_input(slot: &mut Option<PathBuf>, p: PathBuf) -> Result<(), String> {
    if let Some(prev) = slot {
        return Err(format!(
            "unexpected argument `{}` (input is already `{}`; program arguments go after `--`)",
            p.display(),
            prev.display()
        ));
    }
    *slot = Some(p);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::p;
    use super::super::Command;
    use super::*;

    fn build(args: &[&str]) -> BuildArgs {
        match p(args).unwrap() {
            Command::Build(b) => b,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn build_defaults() {
        let b = build(&["build", "main.vlt"]);
        assert_eq!(b.input, Some(PathBuf::from("main.vlt")));
        assert_eq!(
            (b.output, b.release, b.target, b.emit, b.verbose, b.locked),
            (None, false, None, Emit::Exe, false, false)
        );
        assert_eq!(b.backend, None);
        assert_eq!(build(&["build"]).input, None, "no file → package mode");
    }

    #[test]
    fn build_all_flags() {
        let b = build(&[
            "build",
            "-o",
            "out/x",
            "--release",
            "-g",
            "a.vlt",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--emit",
            "vir",
            "-v",
            "--locked",
            "--backend",
            "llvm",
        ]);
        assert_eq!(b.input, Some(PathBuf::from("a.vlt")));
        assert_eq!(b.output, Some(PathBuf::from("out/x")));
        assert!(b.release && b.debug_info && b.verbose && b.locked);
        assert_eq!(b.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
        assert_eq!(b.emit, Emit::Vir);
        assert_eq!(b.backend, Some(Backend::Llvm));
        let b = build(&["build", "a.vlt", "--emit=llvm", "--backend=cranelift"]);
        assert_eq!((b.emit, b.backend), (Emit::Llvm, Some(Backend::Cranelift)));
        let b = build(&["build", "a.vlt", "--timings"]);
        assert!(b.timings && b.verbose, "--timings implies -v");
        match p(&["run", "--target", "wasm32-wasip1", "a.vlt"]).unwrap() {
            Command::Run { build, .. } => {
                assert_eq!(build.target.as_deref(), Some("wasm32-wasip1"))
            }
            other => panic!("{other:?}"),
        }
        let b = build(&["build", "a.vlt", "--emit=obj", "--target=t", "--output=o"]);
        assert_eq!(
            (b.emit, b.target.as_deref(), b.output),
            (Emit::Obj, Some("t"), Some(PathBuf::from("o")))
        );
    }

    #[test]
    fn run_forwards_args() {
        match p(&[
            "run",
            "--release",
            "--backend",
            "llvm",
            "a.vlt",
            "--",
            "x",
            "--release",
            "-o",
        ])
        .unwrap()
        {
            Command::Run { build, args } => {
                assert!(build.release);
                assert_eq!(build.backend, Some(Backend::Llvm));
                assert_eq!(build.emit, Emit::Exe);
                assert_eq!(args, ["x", "--release", "-o"].map(OsString::from));
            }
            other => panic!("{other:?}"),
        }
    }

    /// As `node file.js a b`: what follows the file is the program's, `--` or not.
    #[test]
    fn run_passes_what_follows_the_file() {
        let run = |args: &[&str]| match p(args).unwrap() {
            Command::Run { build, args } => (build, args),
            other => panic!("{other:?}"),
        };
        let (b, args) = run(&["run", "--release", "a.vlt", "x", "--release", "-h"]);
        assert!(b.release);
        assert_eq!(b.input, Some(PathBuf::from("a.vlt")));
        assert_eq!(args, ["x", "--release", "-h"].map(OsString::from));
        let (b, args) = run(&["run", "a.vlt", "-v"]);
        assert!(!b.verbose, "`-v` after the file is the program's");
        assert_eq!(args, ["-v"].map(OsString::from));
        // A `--` right after the file separates as before; a later one is the program's.
        let (_, args) = run(&["run", "a.vlt", "--", "a", "--", "b"]);
        assert_eq!(args, ["a", "--", "b"].map(OsString::from));
        // Without a file (a package), program arguments still need `--`.
        let (b, args) = run(&["run", "--", "a.vlt"]);
        assert_eq!((b.input, args), (None, vec![OsString::from("a.vlt")]));
        assert_eq!(
            run_options(&["--backend", "llvm", "-h", "a.vlt", "--help"].map(OsString::from)),
            ["--backend", "llvm", "-h"].map(OsString::from)
        );
    }

    #[test]
    fn errors() {
        assert!(p(&["build", "a.vlt", "b.vlt"])
            .unwrap_err()
            .contains("unexpected argument"));
        assert!(p(&["build", "a.vlt", "--emit", "asm"])
            .unwrap_err()
            .contains("--emit"));
        assert!(p(&["build", "a.vlt", "--backend", "gcc"])
            .unwrap_err()
            .contains("unknown backend"));
        assert!(p(&["build", "a.vlt", "-o"])
            .unwrap_err()
            .contains("expects a value"));
        assert!(p(&["build", "a.vlt", "--bogus"])
            .unwrap_err()
            .contains("unknown option"));
        assert!(
            p(&["run", "a.vlt", "-o", "x"]).is_ok(),
            "the program's options"
        );
        assert!(p(&["run", "-o", "x", "a.vlt"])
            .unwrap_err()
            .contains("unknown option"));
        assert!(p(&["build", "a.vlt", "extra"])
            .unwrap_err()
            .contains("after `--`"));
    }
}
