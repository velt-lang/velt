//! `velt check`: the front end only (load, parse, sema) on a file and its imports or on every
//! module of the current package, for fast feedback while editing and for tools (`--json`).
//! Nothing is lowered, compiled or linked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::lint;
use super::project::Project;
use super::test::discover;
use crate::cli::{BuildArgs, CheckArgs, Emit, FixMode};
use crate::driver::{self, BuildError, CheckScope, Session};
use serde_json::{json, Value};
use velt_common::{Diagnostic, Severity, SourceMap, Span};

/// Directories of a package whose modules `velt check` checks (recursively). Other directories
/// (`examples/`, `bench/`, scripts at the package root) hold standalone programs with their own
/// `main`, checked one at a time with `velt check <file>`.
const PACKAGE_DIRS: [&str; 2] = ["src", "tests"];

/// `velt check`: exit 0 when there are no errors, 1 when there are, 101 on an internal error.
/// A lint finding at the error level (a ts-compat error, a `perf/…` rule at `deny`) is an error
/// too.
pub fn check_command(args: &CheckArgs) -> ExitCode {
    if let Some(mode) = args.fix {
        return fix_command(args, mode);
    }
    let mut sess = Session::new();
    let (result, lint) = check_and_lint(&mut sess, args, false);
    report_and_exit(&sess, args, result, &lint, None)
}

/// The check's outcome and its lint findings: `--ts-compat`'s, or the `perf/…` rules' when
/// [`lint::Package::lints`] (or `fixing`: for `--fix`).
fn check_and_lint(
    sess: &mut Session,
    args: &CheckArgs,
    fixing: bool,
) -> (Result<(), BuildError>, Vec<Diagnostic>) {
    if let Some(paths) = &args.ts_compat {
        let (result, findings) = super::ts_compat::run(sess, args, paths);
        let package = match paths.first() {
            Some(p) if p.is_dir() => lint::Package::in_dir(p),
            Some(p) => lint::Package::around(p),
            None => lint::Package::in_dir(Path::new(".")),
        };
        let diags = findings
            .iter()
            .filter_map(|f| velt_lint::ts_compat(f, &package.levels))
            .collect();
        let linted: Vec<velt_common::FileId> = findings.iter().map(|f| f.span.file).collect();
        let ran = |code: &str| code.starts_with("ts-compat/");
        return (result, velt_lint::finish(diags, &sess.sm, &linted, &ran));
    }
    let (opts, scope, same_path) = match resolve(args) {
        Ok(r) => r,
        Err(e) => return (Err(e), vec![]),
    };
    let package = lint::Package::around(&opts.input);
    if args.std || !(fixing || package.lints(args)) {
        let checked = driver::check_with(sess, &opts, &scope);
        report_same_path(sess, &same_path, &opts.input);
        return (with_same_path(checked, &same_path), vec![]);
    }
    match driver::check_with_modules(sess, &opts, &scope) {
        Ok((loaded, checked)) => {
            report_same_path(sess, &same_path, &opts.input);
            let checked = with_same_path(checked, &same_path);
            let findings = lint::findings(sess, args, &opts, &loaded, checked.is_ok(), &package);
            (checked, findings)
        }
        Err(e) => (Err(e), vec![]),
    }
}

fn with_same_path(
    checked: Result<(), BuildError>,
    same_path: &[Vec<PathBuf>],
) -> Result<(), BuildError> {
    match checked {
        Ok(()) if !same_path.is_empty() => Err(BuildError::Diagnostics),
        other => other,
    }
}

/// Print the check's diagnostics and `lint`'s (text on stderr, or `--json`), then the exit code.
fn report_and_exit(
    sess: &Session,
    args: &CheckArgs,
    result: Result<(), BuildError>,
    lint: &[Diagnostic],
    fixed: Option<&lint::Fixed>,
) -> ExitCode {
    if args.verbose {
        eprint!("{}", sess.render_timings());
    }
    if args.json {
        let failure = match &result {
            Err(BuildError::Failed(msg)) => Some(msg.as_str()),
            _ => None,
        };
        let mut report = report_json(sess, failure, lint);
        if let Some(fixed) = fixed {
            report["fixed"] =
                json!({"fixes": fixed.fixes, "files": fixed.files, "undone": fixed.undone});
        }
        println!("{report}");
    } else {
        if let Some(fixed) = fixed {
            report_fixed(fixed);
        }
        super::build::report(sess, false);
        report_findings(sess, lint);
        if result.is_ok() && !lint.iter().any(Diagnostic::is_error) {
            print!("{}", crate::native::render_native(&sess.native));
        }
    }
    let lint_errors = lint.iter().any(Diagnostic::is_error);
    match result {
        Ok(()) if lint_errors => ExitCode::from(1),
        Ok(()) => ExitCode::SUCCESS,
        Err(BuildError::Failed(_)) if args.json => ExitCode::from(1),
        Err(e) => super::build::failure_code(&e),
    }
}

/// `velt check --fix`: check and lint, apply the fixes `mode` picks, then check and lint again
/// and report what is left, after what was fixed.
fn fix_command(args: &CheckArgs, mode: FixMode) -> ExitCode {
    let plain = CheckArgs {
        fix: None,
        ..args.clone()
    };
    let mut sess = Session::new();
    let (result, lint) = check_and_lint(&mut sess, &plain, true);
    if let Err(BuildError::Failed(_) | BuildError::Ice(_)) = &result {
        return report_and_exit(&sess, &plain, result, &lint, None);
    }
    let errors = |s: &Session| s.diagnostics.iter().filter(|d| d.is_error()).count();
    let before = errors(&sess);
    // A check without the lint: the fixes must not add errors (the evidence rules would build
    // the program again for nothing).
    let check_only = CheckArgs {
        perf: false,
        ..plain.clone()
    };
    let recheck = || {
        let mut s = Session::new();
        let (result, _) = match &check_only.ts_compat {
            Some(paths) => super::ts_compat::run(&mut s, &check_only, paths),
            None => (check_input_or_package(&mut s, &check_only), vec![]),
        };
        !matches!(result, Err(BuildError::Failed(_) | BuildError::Ice(_))) && errors(&s) <= before
    };
    let candidates: Vec<Diagnostic> = sess.diagnostics.iter().cloned().chain(lint).collect();
    let fixed = match lint::apply(&sess, &candidates, mode, &recheck) {
        Ok(fixed) => fixed,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(1);
        }
    };
    let mut sess = Session::new();
    let (result, lint) = check_and_lint(&mut sess, &plain, true);
    report_and_exit(&sess, &plain, result, &lint, Some(&fixed))
}

/// What `--fix` did, on stderr.
fn report_fixed(fixed: &lint::Fixed) {
    let s = |n: usize| if n == 1 { "" } else { "s" };
    if fixed.fixes == 0 && fixed.undone.is_empty() {
        eprintln!("velt: nothing to fix");
    } else {
        eprintln!(
            "velt: fixed {} problem{} in {} file{}",
            fixed.fixes,
            s(fixed.fixes),
            fixed.files,
            s(fixed.files)
        );
    }
    for title in &fixed.undone {
        eprintln!("velt: not applied, the program did not check after it: {title}");
    }
}

/// Check the current package for `target`, as `velt check --target <target>` does, into `sess`.
pub(super) fn check_package_for(sess: &mut Session, target: &str) -> Result<(), BuildError> {
    let args = CheckArgs {
        target: Some(target.to_string()),
        ..Default::default()
    };
    check_input_or_package(sess, &args)
}

/// `velt check [<file>]`: the file and its imports, or the current package.
fn check_input_or_package(sess: &mut Session, args: &CheckArgs) -> Result<(), BuildError> {
    resolve(args).and_then(|(opts, scope, same_path)| {
        let checked = driver::check_with(sess, &opts, &scope);
        report_same_path(sess, &same_path, &opts.input);
        with_same_path(checked, &same_path)
    })
}

/// The lint findings on stderr, after the check's diagnostics.
fn report_findings(sess: &Session, lint: &[Diagnostic]) {
    let rendered: Vec<String> = lint.iter().map(|d| d.render(&sess.sm)).collect();
    if !rendered.is_empty() {
        // A blank line after the check's diagnostics, as between them.
        if !sess.diagnostics.is_empty() {
            eprintln!();
        }
        eprintln!("{}", rendered.join("\n\n"));
    }
}

/// The file (inside its package, if any) and its imports, or the current package's modules,
/// resolved like `velt build` (package dependencies are installed if needed); plus, for a
/// package, its [`same_path_groups`].
fn resolve(
    args: &CheckArgs,
) -> Result<(driver::BuildOptions, CheckScope, Vec<Vec<PathBuf>>), BuildError> {
    if let Some(target) = &args.target {
        check_target(target)?;
    }
    if args.std {
        return std_scope(args.locked, args.target.clone());
    }
    let (input, scope, same_path) = match &args.input {
        Some(file) => {
            super::project::check_input_file(file).map_err(BuildError::Failed)?;
            (file.clone(), CheckScope::default(), vec![])
        }
        None => package_scope().map_err(BuildError::Failed)?,
    };
    let mut opts = options_for(&input, args.locked)?;
    opts.no_generators = args.no_generators;
    opts.target = args.target.clone();
    let scope = without_other_variants(scope, &opts);
    let scope = match std::env::var_os("VELT_STD_SUMMARY_EAGER") {
        Some(_) => with_all_std(scope, &opts)?,
        None => scope,
    };
    Ok((opts, scope, same_path))
}

/// `velt check --std`: every file of the standard library (its tests aside), loaded as std code
/// below an empty root, each definition checked whether or not anything reaches it.
fn std_scope(
    locked: bool,
    target: Option<String>,
) -> Result<(driver::BuildOptions, CheckScope, Vec<Vec<PathBuf>>), BuildError> {
    let std = velt_loader::std_root().ok_or_else(|| {
        BuildError::Failed("cannot find the standard library; set VELT_STD".into())
    })?;
    let mut extra_roots = std_files(&std)?;
    let imports = std.join(vpm::imports::STD_IMPORTS_FILE);
    let input = std.join("__velt_check_std.velt");
    let mut opts = options_for(&input, locked)?;
    opts.root_source = Some(String::new());
    opts.target = target;
    // std's own `#name` variants that the target does not use, as for a package.
    if let Ok(src) = std::fs::read_to_string(&imports) {
        let map = vpm::manifest::read::read_std_imports(velt_common::FileId(0), &src);
        let unused = unselected_variants(&std, &map.unwrap_or_default(), &opts.target());
        extra_roots.retain(|f| !unused.contains(&file_key(f)));
    }
    let scope = CheckScope {
        require_main: false,
        extra_roots,
        eager_std: true,
    };
    Ok((opts, scope, vec![]))
}

/// `--target`'s triple is one velt builds for: x86_64 or aarch64 on Windows, macOS or Linux, or
/// one of the WebAssembly targets (a typo would otherwise check for the conditions of `native`).
fn check_target(target: &str) -> Result<(), BuildError> {
    let rest = target
        .strip_prefix("x86_64-")
        .or_else(|| target.strip_prefix("aarch64-"));
    let native = rest.is_some_and(|rest| {
        matches!(
            rest,
            "pc-windows-msvc" | "unknown-linux-gnu" | "unknown-linux-musl"
        ) || rest.starts_with("apple-darwin")
            || rest.starts_with("apple-macos")
    });
    if native || super::wasm::is_wasm(target) {
        return Ok(());
    }
    Err(BuildError::Failed(format!(
        "unsupported target `{target}` (velt builds for x86_64 and aarch64 on Windows, macOS and \
         Linux, wasm32-wasip1 and wasm32-unknown-unknown)"
    )))
}

/// Build options for checking `input` (inside its package, if any), comparing the variants of
/// its `#name` imports.
pub(super) fn options_for(input: &Path, locked: bool) -> Result<driver::BuildOptions, BuildError> {
    let build = BuildArgs {
        input: Some(input.to_path_buf()),
        locked,
        // No backend is resolved for IR output: checking needs neither clang nor a linker.
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut opts = super::build::build_options(&build).map_err(BuildError::Failed)?;
    opts.check_variants = true;
    Ok(opts)
}

/// `scope` without the variants of the package's `#name` imports that this target does not
/// select: they are compared with the selected one instead of checked as code of this build,
/// which they may not compile for (`velt check --target` checks them for theirs).
fn without_other_variants(mut scope: CheckScope, opts: &driver::BuildOptions) -> CheckScope {
    let Some(graph) = &opts.packages else {
        return scope;
    };
    let Some((_, root, imports)) = graph.imports_of(&opts.input) else {
        return scope;
    };
    if imports.is_empty() {
        return scope;
    }
    let unused = unselected_variants(root, imports, &opts.target());
    scope.extra_roots.retain(|f| !unused.contains(&file_key(f)));
    scope
}

/// Every file of the standard library at `std` (its tests and its import map aside), sorted.
fn std_files(std: &Path) -> Result<Vec<PathBuf>, BuildError> {
    let mut files: Vec<PathBuf> = discover::source_files(std)
        .map_err(BuildError::Failed)?
        .into_iter()
        .filter(|f| {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned());
            name.is_some_and(|n| n.ends_with(".velt") && !n.ends_with(".test.velt"))
        })
        .collect();
    // std's conditional imports are data the loader reads, as a manifest is, not a module.
    let imports = std.join(vpm::imports::STD_IMPORTS_FILE);
    files.retain(|f| *f != imports);
    files.sort();
    Ok(files)
}

/// `VELT_STD_SUMMARY_EAGER` (the std identity oracle's whole-std mode, #2943): the program with
/// every std file, each std definition checked whether or not the program reaches it, as a cache
/// of checked std holds std. A source generator's host is built apart and doesn't see it.
fn with_all_std(
    mut scope: CheckScope,
    opts: &driver::BuildOptions,
) -> Result<CheckScope, BuildError> {
    let std = velt_loader::std_root().ok_or_else(|| {
        BuildError::Failed("cannot find the standard library; set VELT_STD".into())
    })?;
    let imports = std.join(vpm::imports::STD_IMPORTS_FILE);
    let mut files = std_files(&std)?;
    if let Ok(src) = std::fs::read_to_string(&imports) {
        let map = vpm::manifest::read::read_std_imports(velt_common::FileId(0), &src);
        let unused = unselected_variants(&std, &map.unwrap_or_default(), &opts.target());
        files.retain(|f| !unused.contains(&file_key(f)));
    }
    scope.extra_roots.extend(files);
    scope.eager_std = true;
    Ok(scope)
}

/// The files of `imports` (variants relative to `base`) that a build for `target` uses for no
/// `#name`, as [`file_key`]s.
fn unselected_variants(
    base: &Path,
    imports: &vpm::imports::ImportMap,
    target: &str,
) -> Vec<PathBuf> {
    let conditions = vpm::imports::conditions(target, None);
    let file = |t: &str| file_key(&base.join(t.trim_start_matches("./")));
    let used: Vec<PathBuf> = imports
        .values()
        .filter_map(|variants| vpm::imports::select(variants, &conditions))
        .map(|v| file(&v.target))
        .collect();
    imports
        .values()
        .flatten()
        .map(|v| file(&v.target))
        .filter(|f| !used.contains(f))
        .collect()
}

fn file_key(f: &Path) -> PathBuf {
    std::fs::canonicalize(f).unwrap_or_else(|_| f.to_path_buf())
}

/// The whole current package, like `tsc` checks a project: its root module (see
/// [`package_root_module`]) plus every other module under [`PACKAGE_DIRS`] as a library module.
fn package_scope() -> Result<(PathBuf, CheckScope, Vec<Vec<PathBuf>>), String> {
    let root = Project::current_root()?;
    let (input, require_main) = package_root_module(&root)?;
    let mut extra_roots = vec![];
    for dir in PACKAGE_DIRS
        .map(|d| root.join(d))
        .iter()
        .filter(|d| d.is_dir())
    {
        extra_roots.extend(discover::source_files(dir)?);
    }
    let same_path = same_path_groups(&extra_roots);
    extra_roots.retain(|f| *f != input);
    let scope = CheckScope {
        require_main,
        extra_roots,
        eager_std: false,
    };
    Ok((input, scope, same_path))
}

/// The files among `files` that share a directory and a module path (`dup.velt` and `dup.ts`),
/// in groups of two or more: an import of `./dup` is ambiguous, and they cannot both be the
/// module `dup`.
pub(super) fn same_path_groups(files: &[PathBuf]) -> Vec<Vec<PathBuf>> {
    let mut groups: BTreeMap<(PathBuf, String), Vec<PathBuf>> = BTreeMap::new();
    for file in files {
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Some(stem) = vpm::sources::strip_source_extension(name) {
            let dir = file.parent().unwrap_or(Path::new("")).to_path_buf();
            groups
                .entry((dir, stem.to_string()))
                .or_default()
                .push(file.clone());
        }
    }
    groups.into_values().filter(|g| g.len() > 1).collect()
}

/// One error per [`same_path_groups`] group, located in its first file and naming every file
/// relative to the package root (the directory of `input`'s package).
pub(super) fn report_same_path(sess: &mut Session, groups: &[Vec<PathBuf>], input: &Path) {
    let root = vpm::manifest::find_package_root(input.parent().unwrap_or(Path::new("")));
    let shown = |f: &Path| match &root {
        Some(r) => vpm::relpath::relative(&vpm::relpath::absolute(f), r),
        None => f.display().to_string(),
    };
    for group in groups {
        let mut names: Vec<String> = group.iter().map(|f| format!("`{}`", shown(f))).collect();
        let last = names.pop().unwrap_or_default();
        let msg = format!(
            "{} and {last} have the same module path: an import cannot tell them apart",
            names.join(", ")
        );
        let src = std::fs::read_to_string(&group[0]).unwrap_or_default();
        let span = Span::new(sess.sm.add(&group[0], src), 0, 0);
        let note = "rename or remove all but one of them";
        sess.diagnostics
            .push(Diagnostic::error(msg, span).with_note(note));
    }
}

/// The root module of the package at `root` and whether it must define `main`: its runnable
/// entry (`package.entry`, default `src/main.velt`, `.ts` or `.tsx`; `main` required), or
/// `src/lib.velt` (`.ts`, `.tsx`) for a library package (one without a configured entry and
/// without a default one). A configured entry that is missing is an error, as for `velt build`.
fn package_root_module(root: &Path) -> Result<(PathBuf, bool), String> {
    let manifest = vpm::Manifest::from_dir(root)?;
    let entry = &manifest.package.entry;
    if entry != vpm::manifest::DEFAULT_ENTRY {
        let file = root.join(entry);
        return match file.is_file() {
            true => Ok((file, true)),
            false => Err(format!(
                "package `{}` has no `{entry}` to check",
                manifest.package.name
            )),
        };
    }
    if let Some(file) = vpm::sources::default_module(root, entry)? {
        return Ok((file, true));
    }
    match vpm::sources::default_module(root, vpm::manifest::LIB_ENTRY)? {
        Some(file) => Ok((file, false)),
        None => Err(format!(
            "package `{}` has neither `{entry}` nor `{}` to check",
            manifest.package.name,
            vpm::manifest::LIB_ENTRY
        )),
    }
}

/// `{"diagnostics": [...], "errors": n, "warnings": n}`, and `native` when the check passed and
/// the program allows packages with native code (their libraries and unsafe code); a non-source failure (unreadable
/// root, broken package) becomes an error diagnostic without a location. Lint findings follow the
/// check's diagnostics.
pub(crate) fn report_json(sess: &Session, failure: Option<&str>, lint: &[Diagnostic]) -> Value {
    let mut diagnostics: Vec<Value> = sess
        .diagnostics
        .iter()
        .map(|d| diagnostic_json(d, &sess.sm))
        .collect();
    diagnostics.extend(failure.map(|msg| {
        json!({"severity": "error", "message": msg, "location": null, "labels": [], "notes": [],
               "code": null, "fix": null, "fixes": []})
    }));
    diagnostics.extend(lint.iter().map(|d| diagnostic_json(d, &sess.sm)));
    let count = |sev: &str| diagnostics.iter().filter(|d| d["severity"] == sev).count();
    let (errors, warnings) = (count("error"), count("warning"));
    let mut report = json!({"diagnostics": diagnostics, "errors": errors, "warnings": warnings});
    if errors == 0 && failure.is_none() && !sess.native.is_empty() {
        report["native"] = crate::native::native_json(&sess.native);
    }
    report
}

/// One diagnostic: the first label is the primary `location`; the other labels (with their
/// messages) follow in `labels`. `code` is the lint rule's (`null` for the compiler's own),
/// `fixes` every fix (`{"title", "applicability": "safe"|"review", "edits": [{"location",
/// "replacement"}]}`), and `fix` the first one when it is one edit, as `{"location",
/// "replacement", "title"}`, else `null`.
fn diagnostic_json(d: &Diagnostic, sm: &SourceMap) -> Value {
    let severity = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
    };
    let primary = d
        .labels
        .first()
        .map_or(Value::Null, |l| location(sm, l.span));
    let labels: Vec<Value> = d
        .labels
        .iter()
        .skip(1)
        .map(|l| json!({"location": location(sm, l.span), "message": l.message}))
        .collect();
    let fixes: Vec<Value> = d
        .fixes
        .iter()
        .map(|f| {
            let edits: Vec<Value> = (f.edits.iter())
                .map(|(s, text)| json!({"location": location(sm, *s), "replacement": text}))
                .collect();
            let applicability = match f.applicability {
                velt_common::Applicability::Safe => "safe",
                velt_common::Applicability::Review => "review",
            };
            json!({"title": f.title, "applicability": applicability, "edits": edits})
        })
        .collect();
    let fix = match d.fixes.first() {
        Some(f) if f.edits.len() == 1 => json!({
            "location": location(sm, f.edits[0].0),
            "replacement": f.edits[0].1,
            "title": f.title,
        }),
        _ => Value::Null,
    };
    json!({
        "severity": severity,
        "message": d.message,
        "location": primary,
        "labels": labels,
        "notes": d.notes,
        "code": d.code.as_deref(),
        "fix": fix,
        "fixes": fixes,
    })
}

/// `{"file", "line", "column", "endLine", "endColumn"}` (1-based, columns in bytes, like the text
/// output), or `null` for a span outside the source map.
fn location(sm: &SourceMap, span: Span) -> Value {
    let Some((_, file)) = sm.files().nth(span.file.0 as usize) else {
        return Value::Null;
    };
    let (line, column) = sm.line_col(span.file, span.lo);
    let (end_line, end_column) = sm.line_col(span.file, span.hi);
    json!({
        "file": file.path.to_string_lossy(),
        "line": line,
        "column": column,
        "endLine": end_line,
        "endColumn": end_column,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_has_locations_labels_and_counts() {
        let mut sess = Session::new();
        let file = sess.sm.add("a.velt", "let x = 1;\nlet y = x;\n");
        sess.diagnostics.push(
            Diagnostic::error("bad", Span::new(file, 15, 16))
                .with_label(Span::new(file, 4, 5), "declared here")
                .with_note("a note"),
        );
        sess.diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            ..Diagnostic::error("meh", Span::new(file, 0, 3))
        });
        let v = report_json(&sess, Some("cannot read b.velt"), &[]);
        assert_eq!(
            (v["errors"].as_u64(), v["warnings"].as_u64()),
            (Some(2), Some(1))
        );
        let d = &v["diagnostics"][0];
        assert_eq!(d["location"]["file"], "a.velt");
        assert_eq!(
            (
                d["location"]["line"].as_u64(),
                d["location"]["column"].as_u64()
            ),
            (Some(2), Some(5))
        );
        assert_eq!(d["location"]["endColumn"].as_u64(), Some(6));
        assert_eq!(d["labels"][0]["message"], "declared here");
        assert_eq!(d["labels"][0]["location"]["column"].as_u64(), Some(5));
        assert_eq!(d["notes"][0], "a note");
        assert!(v["diagnostics"][2]["location"].is_null());
    }
}
