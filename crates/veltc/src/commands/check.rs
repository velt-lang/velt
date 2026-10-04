//! `velt check`: the front end only (load, parse, sema) on a file and its imports or on every
//! module of the current package, for fast feedback while editing and for tools (`--json`).
//! Nothing is lowered, compiled or linked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{json, Value};
use velt_common::{Diagnostic, Severity, SourceMap, Span};
use velt_tscompat::Finding;

use super::project::Project;
use super::test::discover;
use crate::cli::{BuildArgs, CheckArgs, Emit};
use crate::driver::{self, BuildError, CheckScope, Session};

/// Directories of a package whose modules `velt check` checks (recursively). Other directories
/// (`examples/`, `bench/`, scripts at the package root) hold standalone programs with their own
/// `main`, checked one at a time with `velt check <file>`.
const PACKAGE_DIRS: [&str; 2] = ["src", "tests"];

/// `velt check`: exit 0 when there are no errors, 1 when there are, 101 on an internal error.
/// With `--ts-compat`, an error-severity finding is an error too.
pub fn check_command(args: &CheckArgs) -> ExitCode {
    let mut sess = Session::new();
    let (result, findings) = match &args.ts_compat {
        Some(paths) => super::ts_compat::run(&mut sess, args, paths),
        None => (check_input_or_package(&mut sess, args), vec![]),
    };
    if args.verbose {
        eprint!("{}", sess.render_timings());
    }
    if args.json {
        let failure = match &result {
            Err(BuildError::Failed(msg)) => Some(msg.as_str()),
            _ => None,
        };
        println!("{}", report_json(&sess, failure, &findings));
    } else {
        super::build::report(&sess, false);
        report_findings(&sess, &findings);
    }
    let lint_errors = findings
        .iter()
        .any(|f| f.severity == velt_tscompat::Severity::Error);
    match result {
        Ok(()) if lint_errors => ExitCode::from(1),
        Ok(()) => ExitCode::SUCCESS,
        Err(BuildError::Failed(_)) if args.json => ExitCode::from(1),
        Err(e) => super::build::failure_code(&e),
    }
}

/// `velt check [<file>]`: the file and its imports, or the current package.
fn check_input_or_package(sess: &mut Session, args: &CheckArgs) -> Result<(), BuildError> {
    resolve(args).and_then(|(opts, scope, same_path)| {
        let checked = driver::check_with(sess, &opts, &scope);
        report_same_path(sess, &same_path, &opts.input);
        match checked {
            Ok(()) if !same_path.is_empty() => Err(BuildError::Diagnostics),
            other => other,
        }
    })
}

/// The `--ts-compat` findings on stderr, after the check's diagnostics.
fn report_findings(sess: &Session, findings: &[Finding]) {
    let rendered: Vec<String> = findings
        .iter()
        .map(|f| super::ts_compat::diagnostic(f).render(&sess.sm))
        .collect();
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
    let (input, scope, same_path) = match &args.input {
        Some(file) => {
            super::project::check_input_file(file).map_err(BuildError::Failed)?;
            (file.clone(), CheckScope::default(), vec![])
        }
        None => package_scope().map_err(BuildError::Failed)?,
    };
    let opts = options_for(&input, args.locked)?;
    Ok((opts, scope, same_path))
}

/// Build options for checking `input` (inside its package, if any).
pub(super) fn options_for(input: &Path, locked: bool) -> Result<driver::BuildOptions, BuildError> {
    let build = BuildArgs {
        input: Some(input.to_path_buf()),
        locked,
        // No backend is resolved for IR output: checking needs neither clang nor a linker.
        emit: Emit::Vir,
        ..Default::default()
    };
    super::build::build_options(&build).map_err(BuildError::Failed)
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
    };
    Ok((input, scope, same_path))
}

/// The files among `files` that share a directory and a module path (`dup.vlt` and `dup.ts`),
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
/// entry (`package.entry`, default `src/main.vlt`, `.ts` or `.tsx`; `main` required), or
/// `src/lib.vlt` (`.ts`, `.tsx`) for a library package (one without a configured entry and
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

/// `{"diagnostics": [...], "errors": n, "warnings": n}`; a non-source failure (unreadable root,
/// broken package) becomes an error diagnostic without a location. `--ts-compat` findings
/// follow the check's diagnostics, with their `code` and `fix` (both `null` elsewhere).
fn report_json(sess: &Session, failure: Option<&str>, findings: &[Finding]) -> Value {
    let mut diagnostics: Vec<Value> = sess
        .diagnostics
        .iter()
        .map(|d| diagnostic_json(d, &sess.sm))
        .collect();
    diagnostics.extend(failure.map(|msg| {
        json!({"severity": "error", "message": msg, "location": null, "labels": [], "notes": [],
               "code": null, "fix": null})
    }));
    diagnostics.extend(findings.iter().map(|f| finding_json(f, &sess.sm)));
    let count = |sev: &str| diagnostics.iter().filter(|d| d["severity"] == sev).count();
    let (errors, warnings) = (count("error"), count("warning"));
    json!({"diagnostics": diagnostics, "errors": errors, "warnings": warnings})
}

/// A finding as a diagnostic, with its `code` and `fix` (`{"location", "replacement",
/// "title"}` or `null`).
fn finding_json(f: &Finding, sm: &SourceMap) -> Value {
    let mut d = diagnostic_json(&super::ts_compat::diagnostic(f), sm);
    d["code"] = json!(f.code);
    if let Some(fix) = &f.fix {
        d["fix"] = json!({
            "location": location(sm, fix.span),
            "replacement": fix.replacement,
            "title": fix.title,
        });
    }
    d
}

/// One diagnostic: the first label is the primary `location`; the other labels (with their
/// messages) follow in `labels`.
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
    json!({
        "severity": severity,
        "message": d.message,
        "location": primary,
        "labels": labels,
        "notes": d.notes,
        "code": null,
        "fix": null,
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
        let file = sess.sm.add("a.vlt", "let x = 1;\nlet y = x;\n");
        sess.diagnostics.push(
            Diagnostic::error("bad", Span::new(file, 15, 16))
                .with_label(Span::new(file, 4, 5), "declared here")
                .with_note("a note"),
        );
        sess.diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            ..Diagnostic::error("meh", Span::new(file, 0, 3))
        });
        let v = report_json(&sess, Some("cannot read b.vlt"), &[]);
        assert_eq!(
            (v["errors"].as_u64(), v["warnings"].as_u64()),
            (Some(2), Some(1))
        );
        let d = &v["diagnostics"][0];
        assert_eq!(d["location"]["file"], "a.vlt");
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
