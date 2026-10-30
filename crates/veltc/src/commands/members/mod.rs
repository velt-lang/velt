//! Commands on several members of a workspace (docs/internals/design/workspaces.md "Running
//! commands on several members"): `velt check`, `test`, `build`, `fmt` and `doc` take
//! `--workspace`, `-p <glob>` and `--affected [<base>]`, and `velt run`/`dev` take `-p <member>`.
//! At a workspace's root, in no package, the first five run on every member.
//!
//! [`intercept`] reads those flags before the command's own parser sees the arguments. `run` and
//! `dev` then continue in the member's directory. The others run as one `velt` per member
//! ([`run`]), in dependency order: a member starts once the members it depends on finished, so
//! it takes their compiled code from the machine's build cache instead of compiling it at the
//! same time. Each member's program, stamp and test results stay in its own `target/`.

mod affected;
mod run;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use vpm::workspace::{Member, Workspace};

use crate::style;

/// What [`intercept`] did with the command line.
pub enum Intercept {
    /// The command ran on the selected members; this is its exit code.
    Done(ExitCode),
    /// Parse and run these arguments as usual (in the selected member's directory, for `run` and
    /// `dev`).
    Continue(Vec<OsString>),
}

/// Commands that run on every selected member.
const MULTI: &[&str] = &["check", "test", "build", "fmt", "doc"];
/// Commands that run one member's program.
const SINGLE: &[&str] = &["run", "dev"];
/// Flags of those commands that take a value, so their value is not taken for a file.
const VALUE_FLAGS: &[&str] = &[
    "-o",
    "--output",
    "--target",
    "--cpu",
    "--emit",
    "--backend",
    "--filter",
    "--jobs",
    "-j",
    "--report",
];

/// The member selection of a command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// `--workspace`.
    pub workspace: bool,
    /// `-p <glob>` / `--package <glob>`, each a glob over member names.
    pub packages: Vec<String>,
    /// `--affected [<base>]`: `Some(None)` compares with the default base.
    pub affected: Option<Option<String>>,
    /// `--fail-fast`.
    pub fail_fast: bool,
}

impl Selection {
    fn is_empty(&self) -> bool {
        !self.workspace && self.packages.is_empty() && self.affected.is_none()
    }
}

/// Take the member selection out of `args` (a command's arguments, without the command): the
/// selection, and the arguments left for the command. Arguments after `--` are the program's.
pub fn extract(args: &[OsString]) -> Result<(Selection, Vec<OsString>), String> {
    let mut sel = Selection::default();
    let mut rest = vec![];
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let Some(text) = arg.to_str() else {
            rest.push(arg.clone());
            continue;
        };
        match text {
            "--" => {
                rest.push(arg.clone());
                rest.extend(iter.by_ref().cloned());
            }
            "--workspace" => sel.workspace = true,
            "--fail-fast" => sel.fail_fast = true,
            "-p" | "--package" => {
                let glob = iter
                    .next()
                    .and_then(|v| v.to_str())
                    .filter(|v| !v.starts_with('-'))
                    .ok_or_else(|| format!("`{text}` needs a member name or glob"))?;
                sel.packages.push(glob.to_string());
            }
            "--affected" => {
                // A base is the next argument when it isn't a flag: files are not allowed with a
                // member selection, so nothing else can follow there.
                let base = iter
                    .next_if(|v| v.to_str().is_some_and(|v| !v.starts_with('-')))
                    .and_then(|v| v.to_str().map(str::to_string));
                sel.affected = Some(base);
            }
            _ => {
                if let Some(glob) = text
                    .strip_prefix("--package=")
                    .or_else(|| text.strip_prefix("-p="))
                {
                    sel.packages.push(glob.to_string());
                } else if let Some(base) = text.strip_prefix("--affected=") {
                    sel.affected = Some(Some(base.to_string()));
                } else {
                    rest.push(arg.clone());
                }
            }
        }
    }
    Ok((sel, rest))
}

/// The first argument of `args` that names a file rather than a flag or a flag's value (before
/// `--`).
fn positional(args: &[OsString]) -> Option<String> {
    let mut iter = args.iter().map(|a| a.to_string_lossy());
    while let Some(arg) = iter.next() {
        if arg == "--" {
            return None;
        }
        if VALUE_FLAGS.contains(&arg.as_ref()) {
            iter.next();
        } else if !arg.starts_with('-') {
            return Some(arg.into_owned());
        }
    }
    None
}

/// The command line `velt` goes on with after [`intercept`] (`velt test -p core` runs as
/// `velt test` in `core`): what a member's test stamp is keyed by, as the workspace command
/// keys its check.
static COMMAND: std::sync::OnceLock<Vec<OsString>> = std::sync::OnceLock::new();

/// The command line this `velt` runs (without the program name), after [`intercept`].
pub fn command_line() -> Vec<OsString> {
    match COMMAND.get() {
        Some(args) => args.clone(),
        None => std::env::args_os().skip(1).collect(),
    }
}

/// Apply the member selection of `args` (the whole command line, without the program name).
pub fn intercept(args: Vec<OsString>) -> Intercept {
    let result = match try_intercept(&args) {
        Ok(Some(done)) => done,
        Ok(None) => Intercept::Continue(args),
        Err(e) => {
            style::error(&e);
            Intercept::Done(ExitCode::from(2))
        }
    };
    if let Intercept::Continue(args) = &result {
        let _ = COMMAND.set(args.clone());
    }
    result
}

fn try_intercept(args: &[OsString]) -> Result<Option<Intercept>, String> {
    let Some(cmd) = args.first().and_then(|a| a.to_str()) else {
        return Ok(None);
    };
    let (multi, single) = (MULTI.contains(&cmd), SINGLE.contains(&cmd));
    if !multi && !single {
        return Ok(None);
    }
    // `velt run <file> <program args>`: only the options before the file (or `--`) are velt's.
    let (mut sel, rest) = if cmd == "run" {
        let options = crate::cli::run_options(&args[1..]);
        let (sel, mut rest) = extract(options)?;
        rest.extend(args[1 + options.len()..].iter().cloned());
        (sel, rest)
    } else {
        extract(&args[1..])?
    };
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    let ws_root = vpm::workspace::find_root(&cwd);
    if sel.is_empty() {
        // At a workspace's root, or between it and its members: every member (`run` and `dev`
        // need one named).
        let in_package = vpm::manifest::find_package_root(&cwd)
            .is_some_and(|pkg| ws_root.as_ref().is_none_or(|ws| pkg.starts_with(ws)));
        let Some(root) = ws_root
            .as_ref()
            .filter(|_| !in_package && positional(&rest).is_none())
        else {
            return Ok(None);
        };
        if single {
            return Err(one_member(cmd, &Workspace::load(root)?, None));
        }
        sel.workspace = true;
    }
    let ws_root = ws_root.ok_or_else(|| {
        format!(
            "`--workspace`, `-p` and `--affected` select members of a workspace, and there is no {} in `{}` or above it",
            vpm::workspace::WORKSPACE_FILE,
            cwd.display()
        )
    })?;
    if let Some(file) = positional(&rest) {
        return Err(format!(
            "with `--workspace`, `-p` or `--affected`, `velt {cmd}` runs on each selected member's package; leave out `{file}`"
        ));
    }
    let ws = Workspace::load(&ws_root)?;
    let mut selected = select(&ws, &sel)?;
    // A library has no program: the members that use it build it, `velt check` checks it.
    if cmd == "build" && selected.len() > 1 {
        let (programs, libraries): (Vec<&Member>, Vec<&Member>) = std::mem::take(&mut selected)
            .into_iter()
            .partition(|m| runnable(m));
        if !libraries.is_empty() {
            let names: Vec<&str> = libraries.iter().map(|m| m.name()).collect();
            style::status(
                "Skipping",
                &format!(
                    "{} (libraries: the members that use them build them, `velt check` checks them)",
                    names.join(", ")
                ),
            );
        }
        selected = programs;
    }
    // One member (always, for `run` and `dev`): the command runs here, in its directory.
    if single || selected.len() == 1 {
        let [member] = selected.as_slice() else {
            return Err(one_member(cmd, &ws, Some(selected.len())));
        };
        // A relative `-o` still means a path from where `velt` was started.
        let rest = absolute_output(rest, &cwd);
        std::env::set_current_dir(&member.dir)
            .map_err(|e| format!("cannot enter `{}`: {e}", member.dir.display()))?;
        let mut args = vec![OsString::from(cmd)];
        args.extend(rest);
        let name = format!("{} ({})", member.name(), shown(&ws, &member.dir));
        // As for several members (`run`): one whose tests can't have changed passes again.
        if multi && cmd == "test" {
            if let Some(tests) = crate::commands::test::member_stamp::up_to_date(&member.dir, &args)
            {
                style::status("Unchanged", &format!("{name}: {tests} passed"));
                return Ok(Some(Intercept::Done(ExitCode::SUCCESS)));
            }
        }
        if multi {
            style::status("Member", &name);
        }
        return Ok(Some(Intercept::Continue(args)));
    }
    if let Some(why) = one_member_only(&rest) {
        return Err(format!(
            "{why} with several members ({} selected); select one with `-p <member>`",
            selected.len()
        ));
    }
    let code = run::run(cmd, &rest, &ws, &selected, sel.fail_fast);
    Ok(Some(Intercept::Done(code)))
}

/// Why an option of `args` can't go with several members, if one can't.
fn one_member_only(args: &[OsString]) -> Option<&'static str> {
    args.iter().find_map(|a| match a.to_str()? {
        "--watch" => Some("`--watch` runs until it is stopped, so it can't run"),
        "-o" | "--output" => Some("`-o` would have every member write the same file"),
        s if s.starts_with("--output=") => Some("`-o` would have every member write the same file"),
        "--json" => Some("`--json` prints one document, not one per member, so it can't run"),
        _ => None,
    })
}

/// `args` with the value of `-o`/`--output` made absolute against `cwd`.
fn absolute_output(args: Vec<OsString>, cwd: &Path) -> Vec<OsString> {
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let text = arg.to_string_lossy().into_owned();
        if text == "--" {
            out.push(arg);
            out.extend(iter.by_ref());
            break;
        }
        if text == "-o" || text == "--output" {
            out.push(arg);
            if let Some(value) = iter.next() {
                out.push(cwd.join(value).into_os_string());
            }
        } else if let Some(value) = text.strip_prefix("--output=") {
            out.push(format!("--output={}", cwd.join(value).display()).into());
        } else {
            out.push(arg);
        }
    }
    out
}

/// The error for `velt run`/`dev` without exactly one member: `selected` members, or `None`
/// at a workspace's root without a selection.
fn one_member(cmd: &str, ws: &Workspace, selected: Option<usize>) -> String {
    let runnable: Vec<&str> = ws
        .members
        .iter()
        .filter(|m| runnable(m))
        .map(Member::name)
        .collect();
    let runnable = match runnable.is_empty() {
        true => "none".to_string(),
        false => runnable.join(", "),
    };
    let what = match selected {
        None => format!(
            "this is a workspace's root (`{}`)",
            vpm::workspace::WORKSPACE_FILE
        ),
        Some(0) => "the selection names no member".to_string(),
        Some(n) => format!("the selection names {n} members"),
    };
    format!(
        "`velt {cmd}` runs one member's program, and {what}; name one with `-p <member>` (runnable members: {runnable})"
    )
}

/// Whether `m` has a program to run (its entry exists).
fn runnable(m: &Member) -> bool {
    let entry = &m.manifest.package.entry;
    match entry == vpm::manifest::DEFAULT_ENTRY {
        true => vpm::sources::default_module(&m.dir, entry).is_ok_and(|f| f.is_some()),
        false => m.dir.join(entry).is_file(),
    }
}

/// The members `sel` names, in dependency order (a member after the members it depends on).
/// Every `-p` glob must match a member.
pub fn select<'w>(ws: &'w Workspace, sel: &Selection) -> Result<Vec<&'w Member>, String> {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    if sel.packages.is_empty() {
        names.extend(ws.members.iter().map(Member::name));
    }
    for glob in &sel.packages {
        let matched: Vec<&str> = ws
            .members
            .iter()
            .map(Member::name)
            .filter(|n| name_matches(glob, n))
            .collect();
        if matched.is_empty() {
            let all: Vec<&str> = ws.members.iter().map(Member::name).collect();
            return Err(format!(
                "`-p {glob}` matches no member of the workspace (members: {})",
                all.join(", ")
            ));
        }
        names.extend(matched);
    }
    if let Some(base) = &sel.affected {
        let affected = affected::members(ws, base.as_deref())?;
        names.retain(|n| affected.contains(*n));
    }
    Ok(dependency_order(ws)
        .into_iter()
        .filter(|m| names.contains(m.name()))
        .collect())
}

/// Whether `-p <glob>` names the member `name`: its whole name, or for a scoped name
/// (`@org/server`) also the part after the scope (`-p 'server*'`).
fn name_matches(glob: &str, name: &str) -> bool {
    use velt_toolchain::members::segment_match;
    segment_match(glob, name)
        || vpm::manifest::split_scoped(name).is_some_and(|(_, bare)| segment_match(glob, bare))
}

/// The members of `ws` that member `name` depends on with `"workspace"`.
pub fn member_deps<'w>(ws: &'w Workspace, m: &'w Member) -> Vec<&'w str> {
    m.manifest
        .dependencies
        .iter()
        .filter(|(n, d)| d.is_workspace() && ws.member(n).is_some())
        .map(|(n, _)| n.as_str())
        .collect()
}

/// Every member, each after the members it depends on (the workspace has no cycles: `load`
/// rejects them), otherwise in directory order.
fn dependency_order(ws: &Workspace) -> Vec<&Member> {
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let mut out = vec![];
    fn visit<'w>(
        ws: &'w Workspace,
        m: &'w Member,
        done: &mut BTreeSet<&'w str>,
        out: &mut Vec<&'w Member>,
    ) {
        if !done.insert(m.name()) {
            return;
        }
        for dep in member_deps(ws, m) {
            if let Some(d) = ws.member(dep) {
                visit(ws, d, done, out);
            }
        }
        out.push(m);
    }
    for m in &ws.members {
        visit(ws, m, &mut done, &mut out);
    }
    out
}

/// For each member, the members that depend on it, directly or not.
fn dependents(ws: &Workspace) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut direct: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for m in &ws.members {
        for dep in member_deps(ws, m) {
            direct.entry(dep).or_default().insert(m.name());
        }
    }
    let mut out = BTreeMap::new();
    for m in &ws.members {
        let mut seen = BTreeSet::new();
        let mut todo = vec![m.name()];
        while let Some(n) = todo.pop() {
            for &d in direct.get(n).into_iter().flatten() {
                if seen.insert(d) {
                    todo.push(d);
                }
            }
        }
        out.insert(m.name(), seen);
    }
    out
}

/// `dir` relative to the workspace root, for messages.
fn shown(ws: &Workspace, dir: &Path) -> String {
    vpm::relpath::relative(dir, &ws.root)
}

#[cfg(test)]
mod tests;
