//! Hand-rolled argument parsing for the `velt` CLI (see docs/internals/contracts/cli.md). No
//! clap: keeps the binary small and startup fast. [`build`] parses `build`/`run`, [`check`] `check`, [`package`]
//! the vpm and test subcommands plus `new`/`init`, [`fmt`] the formatter, [`tools`] the servers
//! and `doc`; `lsp` takes no arguments besides the conventional `--stdio`, `doctor` and `clean`
//! none.
//! [`help`] holds every command's help text (also the source of [`completions`] and of the flag
//! lists behind [`suggest`]'s "did you mean").

mod build;
mod check;
pub mod completions;
mod dev;
mod fmt;
pub mod help;
mod package;
pub mod registry;
pub mod suggest;
pub mod target;
mod tools;

use std::ffi::OsString;
use std::path::PathBuf;

use crate::backend::Backend;
use crate::playground::PlaygroundArgs;
use crate::templates::{Editor, Template};
pub use check::CheckArgs;
use completions::Shell;

/// `velt doc` arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocArgs {
    /// Files or directories to document; empty → the current package's `src/`.
    pub paths: Vec<PathBuf>,
    /// `--std`: document the standard library.
    pub std: bool,
    /// `-o`: output directory (default `target/doc`, in the package root for packages).
    pub output: Option<PathBuf>,
}

/// `velt registry serve` arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryArgs {
    /// Registry directory (`None`: the local registry, `$VELT_REGISTRY` / `~/.velt/registry`).
    pub dir: Option<PathBuf>,
    /// Address to listen on.
    pub addr: String,
}

/// What `--emit` asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Emit {
    /// Print VIR and stop.
    Vir,
    /// Print LLVM IR and stop.
    Llvm,
    /// Object file only.
    Obj,
    /// Linked executable.
    #[default]
    Exe,
}

/// Arguments shared by `build` and `run`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildArgs {
    /// Root file; `None` → the package around the current directory.
    pub input: Option<PathBuf>,
    /// `-o`.
    pub output: Option<PathBuf>,
    /// `--release`.
    pub release: bool,
    /// `-g`: debug info even with `--release`.
    pub debug_info: bool,
    /// `--target`.
    pub target: Option<String>,
    /// `--emit`.
    pub emit: Emit,
    /// `--backend`; `None` → chosen by [`Backend::resolve`].
    pub backend: Option<Backend>,
    /// `-v` (also set by `--timings`).
    pub verbose: bool,
    /// `--timings`: `-v` plus each stage's breakdown (optimizer passes, codegen steps).
    pub timings: bool,
    /// `--locked`.
    pub locked: bool,
    /// `--report numbers`: list the `number` variables in loops that stay doubles.
    pub report_numbers: bool,
    /// `velt build --json`: the result (output path, debug info, diagnostics) as one JSON
    /// document on stdout instead of text on stderr.
    pub json: bool,
}

/// How `velt dev` runs each version of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevMode {
    /// Supervisor starting `velt dev --host` children (default).
    Jit,
    /// Supervisor starting linked executables (`--exe`).
    Exe,
    /// `--host` (internal): compile, JIT and run the program in this process.
    Host,
}

/// `velt dev` arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevArgs {
    /// Build inputs, as for `velt run`.
    pub build: BuildArgs,
    /// Program arguments (after `--`).
    pub args: Vec<OsString>,
    /// `--exe` / `--host`.
    pub mode: DevMode,
}

/// A parsed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `velt build`.
    Build(BuildArgs),
    /// `velt run`: `build` is always `Emit::Exe`; `args` are forwarded to the program.
    Run {
        build: BuildArgs,
        args: Vec<OsString>,
    },
    /// `velt check`: parse + sema only, diagnostics without building.
    Check(CheckArgs),
    /// `velt dev`: rebuild and restart on changes.
    Dev(DevArgs),
    /// `velt test [path]`.
    Test {
        path: Option<PathBuf>,
        release: bool,
        locked: bool,
        /// `--watch`: rerun on every change.
        watch: bool,
    },
    /// `velt fmt [paths...] [--check]`: no paths → the current package's `src/` (or the cwd).
    Fmt { paths: Vec<PathBuf>, check: bool },
    /// `velt lsp`: language server on stdin/stdout.
    Lsp,
    /// `velt playground`: the browser playground server.
    Playground(PlaygroundArgs),
    /// `velt doc`: HTML API documentation.
    Doc(DocArgs),
    /// `velt registry serve`: the package registry server.
    RegistryServe(RegistryArgs),
    /// `velt doctor`: diagnose the installation.
    Doctor,
    /// `velt new <name> [--template <t>]` (`--lib` = `--template lib`).
    New { name: String, template: Template },
    /// `velt init [--template <t>] [--name <n>] [--force]`: a package in the current directory.
    Init {
        /// Package name; `None` → the directory's name.
        name: Option<String>,
        template: Template,
        /// Overwrite existing files.
        force: bool,
    },
    /// `velt init --editor <e>`: only the editor's files, in the current package (or directory).
    InitEditor(Editor),
    /// `velt clean`: remove the package's `target/`.
    Clean,
    /// `velt completions <shell>`.
    Completions(Shell),
    /// `velt add <name>[@<req>] [--path <dir>]`.
    Add {
        name: String,
        version: Option<String>,
        path: Option<String>,
    },
    /// `velt install [--locked]`.
    Install { locked: bool },
    /// `velt update`: re-resolve ignoring the lockfile.
    Update,
    /// `velt publish [--native-artifacts <dir>] [--native-only]`.
    Publish {
        /// Where prebuilt native bundles are collected from (`<dir>/<triple>/`).
        native_artifacts: Option<PathBuf>,
        /// Add native libraries for new targets to the already published version.
        native_only: bool,
    },
    /// `velt native build [--target <triple>]`: build the package's native library bundle.
    NativeBuild { target: Option<String> },
    /// `velt manifest [--json]`: check the package's manifest, or (`json`) print it as JSON for
    /// other tools.
    Manifest { json: bool },
    /// `velt yank <pkg>@<version> [--undo]`.
    Yank {
        name: String,
        version: String,
        undo: bool,
    },
    /// `velt owner list|add|remove <pkg> [<user>]`.
    Owner {
        action: registry::OwnerAction,
        package: String,
    },
    /// `velt search <text>`.
    Search { query: String, json: bool },
    /// `velt login <registry-url>`: store a token (read from stdin) for that registry.
    Login { url: String },
    /// `velt logout <registry-url>`: forget the stored token of that registry.
    Logout { url: String },
    /// `velt registry owner add|remove <pkg> <user> [--dir <d>]`: an administrator's change.
    RegistryOwner {
        add: bool,
        package: String,
        user: String,
        /// Registry directory (`None`: the local registry).
        dir: Option<PathBuf>,
    },
    /// `velt registry user add|remove|token <name> [--dir <d>] [--open]`.
    RegistryUser {
        action: registry::UserAction,
        name: String,
        /// Registry directory (`None`: the local registry).
        dir: Option<PathBuf>,
    },
    /// `velt target list|add|remove`: target packs for cross-compiling.
    Target(target::TargetAction),
    /// `velt toolchain …`: the launcher's command (#948); a toolchain started directly only
    /// says so.
    Toolchain,
    /// `velt --version`.
    Version,
    /// `velt --help` or no arguments (`None`), `velt help <cmd>` / `velt <cmd> --help` (`Some`).
    Help(Option<String>),
}

/// Parse `args` (without the program name). Errors about a command end with a pointer to its
/// `--help`.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut it = args.into_iter();
    let Some(sub) = it.next() else {
        return Ok(Command::Help(None));
    };
    let rest: Vec<OsString> = it.collect();
    let name = sub.to_string_lossy().into_owned();
    // `velt run file.vlt --help` passes `--help` to the program.
    let own = if name == "run" {
        build::run_options(&rest)
    } else {
        &rest[..]
    };
    if help::find(&name).is_some() && wants_help(own) {
        return Ok(Command::Help(Some(name)));
    }
    parse_command(&name, rest).map_err(|e| match help::find(&name) {
        Some(_) => format!("{e}\n\nFor more information, try `velt {name} --help`."),
        None => e,
    })
}

/// `-h`/`--help` among the arguments before `--` (after it they belong to the program).
fn wants_help(rest: &[OsString]) -> bool {
    rest.iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "-h" || a == "--help")
}

fn parse_command(sub: &str, rest: Vec<OsString>) -> Result<Command, String> {
    match sub {
        "build" => build::parse_build(rest, "build").map(|(b, _)| Command::Build(b)),
        "run" => build::parse_build(rest, "run").map(|(build, args)| Command::Run { build, args }),
        "check" => check::parse_check(rest),
        "dev" => dev::parse_dev(rest),
        "fmt" => fmt::parse_fmt(rest),
        "lsp" => parse_lsp(rest),
        "manifest" => parse_manifest(rest),
        "playground" => tools::parse_playground(rest),
        "doc" => tools::parse_doc(rest),
        "registry" => tools::parse_registry(rest),
        "doctor" | "clean" => no_arguments(sub, rest),
        "completions" => parse_completions(rest),
        "help" => parse_help(rest),
        "--version" | "-V" | "version" => Ok(Command::Version),
        "--help" | "-h" => Ok(Command::Help(None)),
        "test" | "new" | "init" | "add" | "install" | "update" | "publish" | "native" => {
            package::parse(sub, rest)
        }
        "yank" => registry::parse_yank(rest),
        "owner" => registry::parse_owner(rest),
        "search" => registry::parse_search(rest),
        "target" => target::parse_target(rest),
        "toolchain" => Ok(Command::Toolchain),
        "login" | "logout" => registry::parse_login(sub, rest),
        _ => Err(unknown_command(sub)),
    }
}

fn unknown_command(sub: &str) -> String {
    if sub.starts_with('+') {
        return format!(
            "`velt {sub} …` picks a toolchain, which the velt launcher does (<root>/bin/velt, \
             which the installer puts on PATH); this velt was started directly"
        );
    }
    let mut msg = format!("unknown command `{sub}`");
    if sub.starts_with('-') {
        msg = format!("unknown option `{sub}`");
    }
    match suggest::closest(sub, &help::command_names()) {
        Some(c) => msg.push_str(&format!("; did you mean `velt {c}`?")),
        None if vpm::sources::is_source_name(sub) => {
            msg.push_str(&format!(" (to run a file: `velt run {sub}`)"))
        }
        None => {}
    }
    msg.push_str("\n\nRun `velt --help` for the list of commands.");
    msg
}

/// Error for an unknown option `flag` of `velt <cmd>`, with the closest known flag.
pub(crate) fn unknown_option(cmd: &str, flag: &str) -> String {
    let command = cmd.split(' ').next().unwrap_or(cmd);
    let flags = help::find(command).map(|c| c.flags()).unwrap_or_default();
    let flag_name = flag.split_once('=').map_or(flag, |(f, _)| f);
    format!(
        "unknown option `{flag}` for `velt {cmd}`{}",
        suggest::hint(flag_name, &flags)
    )
}

/// Commands without arguments (`doctor`, `clean`).
fn no_arguments(sub: &str, rest: Vec<OsString>) -> Result<Command, String> {
    if let Some(other) = strings(rest)?.first() {
        return Err(format!("unexpected argument `{other}` for `velt {sub}`"));
    }
    Ok(match sub {
        "doctor" => Command::Doctor,
        _ => Command::Clean,
    })
}

/// `velt completions <shell>`.
fn parse_completions(rest: Vec<OsString>) -> Result<Command, String> {
    match strings(rest)?.as_slice() {
        [shell] => Shell::parse(shell).map(Command::Completions),
        [] => {
            Err("missing shell (e.g. `velt completions bash`; also zsh, fish, powershell)".into())
        }
        [_, extra, ..] => Err(format!(
            "unexpected argument `{extra}` for `velt completions`"
        )),
    }
}

/// `velt help [<command>]`.
fn parse_help(rest: Vec<OsString>) -> Result<Command, String> {
    match strings(rest)?.as_slice() {
        [] => Ok(Command::Help(None)),
        [cmd] if help::find(cmd).is_some() => Ok(Command::Help(Some(cmd.clone()))),
        [cmd] => Err(unknown_command(cmd)),
        [_, extra, ..] => Err(format!("unexpected argument `{extra}` for `velt help`")),
    }
}

/// `velt lsp [--stdio]` (editors' LSP clients pass `--stdio`; stdio is the only transport).
fn parse_lsp(args: Vec<OsString>) -> Result<Command, String> {
    match strings(args)?.iter().find(|a| *a != "--stdio") {
        Some(other) => Err(format!("unexpected argument `{other}` for `velt lsp`")),
        None => Ok(Command::Lsp),
    }
}

/// `velt manifest [--json]`.
fn parse_manifest(args: Vec<OsString>) -> Result<Command, String> {
    let args = strings(args)?;
    let mut json = false;
    for arg in &args {
        match arg.as_str() {
            "--json" if !json => json = true,
            a if a.starts_with('-') && a != "--json" => return Err(unknown_option("manifest", a)),
            a => return Err(format!("unexpected argument `{a}` for `velt manifest`")),
        }
    }
    Ok(Command::Manifest { json })
}

/// Split into strings; non-UTF-8 arguments are rejected for these commands.
fn strings(args: Vec<OsString>) -> Result<Vec<String>, String> {
    args.into_iter()
        .map(|a| {
            a.into_string()
                .map_err(|a| format!("argument `{}` is not valid UTF-8", a.to_string_lossy()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn p(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn manifest_args() {
        assert_eq!(
            p(&["manifest", "--json"]).unwrap(),
            Command::Manifest { json: true }
        );
        assert_eq!(p(&["manifest"]).unwrap(), Command::Manifest { json: false });
        let err = p(&["manifest", "foo", "--json"]).unwrap_err();
        assert!(err.contains("unexpected argument `foo`"), "{err}");
        let err = p(&["manifest", "--jsn"]).unwrap_err();
        assert!(
            err.contains("unknown option `--jsn`") && err.contains("--json"),
            "{err}"
        );
        assert!(p(&["manifest", "--json", "--json"]).is_err());
    }

    #[test]
    fn misc_commands() {
        assert_eq!(p(&["--version"]).unwrap(), Command::Version);
        assert_eq!(p(&[]).unwrap(), Command::Help(None));
        assert_eq!(p(&["doctor"]).unwrap(), Command::Doctor);
        assert_eq!(p(&["toolchain", "list"]).unwrap(), Command::Toolchain);
        assert!(p(&["+0.2", "build"]).unwrap_err().contains("velt launcher"));
        assert_eq!(p(&["clean"]).unwrap(), Command::Clean);
        assert!(p(&["doctor", "-x"])
            .unwrap_err()
            .contains("unexpected argument"));
        assert!(p(&["clean", "all"])
            .unwrap_err()
            .contains("unexpected argument"));
        assert!(p(&["frobnicate"]).unwrap_err().contains("unknown command"));
    }

    #[test]
    fn help_for_every_command() {
        for c in help::COMMANDS {
            assert_eq!(
                p(&[c.name, "--help"]).unwrap(),
                Command::Help(Some(c.name.into()))
            );
            assert_eq!(
                p(&["help", c.name]).unwrap(),
                Command::Help(Some(c.name.into()))
            );
        }
        assert_eq!(
            p(&["build", "a.vlt", "-h"]).unwrap(),
            Command::Help(Some("build".into()))
        );
        assert_eq!(p(&["-h"]).unwrap(), Command::Help(None));
        // After `--` or the file, `--help` is the program's.
        assert!(matches!(
            p(&["run", "a.vlt", "--", "--help"]).unwrap(),
            Command::Run { .. }
        ));
        assert!(matches!(
            p(&["run", "a.vlt", "--help"]).unwrap(),
            Command::Run { .. }
        ));
        assert_eq!(
            p(&["run", "-h", "a.vlt"]).unwrap(),
            Command::Help(Some("run".into()))
        );
        assert!(p(&["help", "biuld"])
            .unwrap_err()
            .contains("did you mean `velt build`?"));
    }

    #[test]
    fn typo_suggestions() {
        let err = p(&["biuld"]).unwrap_err();
        assert!(
            err.contains("unknown command `biuld`; did you mean `velt build`?"),
            "{err}"
        );
        assert!(p(&["tset"]).unwrap_err().contains("`velt test`"));
        assert!(p(&["hello.vlt"])
            .unwrap_err()
            .contains("`velt run hello.vlt`"));
        let err = p(&["build", "--relase"]).unwrap_err();
        assert!(
            err.contains("unknown option `--relase` for `velt build`; did you mean `--release`?"),
            "{err}"
        );
        assert!(err.contains("try `velt build --help`"), "{err}");
        assert!(p(&["test", "--wach"]).unwrap_err().contains("`--watch`"));
        assert!(p(&["fmt", "--chek"]).unwrap_err().contains("`--check`"));
        assert!(p(&["new", "x", "--templat", "api"])
            .unwrap_err()
            .contains("`--template`"));
        assert!(p(&["doc", "--sdt"]).unwrap_err().contains("`--std`"));
        assert!(p(&["run", "--emit=vir"])
            .unwrap_err()
            .contains("unknown option"));
    }

    #[test]
    fn completions_args() {
        assert_eq!(
            p(&["completions", "zsh"]).unwrap(),
            Command::Completions(Shell::Zsh)
        );
        assert!(p(&["completions"]).unwrap_err().contains("missing shell"));
        assert!(p(&["completions", "fsh"]).unwrap_err().contains("`fish`"));
    }

    #[test]
    fn lsp_args() {
        assert_eq!(p(&["lsp"]).unwrap(), Command::Lsp);
        assert_eq!(p(&["lsp", "--stdio"]).unwrap(), Command::Lsp);
        assert!(p(&["lsp", "--tcp"])
            .unwrap_err()
            .contains("unexpected argument"));
    }
}
