//! `velt check`: the front end only (load, parse, sema) on a file or the current package, for
//! fast feedback while editing and for tools (`--json`). Nothing is lowered, compiled or linked.

use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{json, Value};
use velt_common::{Diagnostic, Severity, SourceMap, Span};

use super::project::Project;
use crate::cli::{BuildArgs, CheckArgs, Emit};
use crate::driver::{self, BuildError, Session};

/// `velt check`: exit 0 when there are no errors, 1 when there are, 101 on an internal error.
pub fn check_command(args: &CheckArgs) -> ExitCode {
    let mut sess = Session::new();
    let result = resolve(args).and_then(|opts| driver::check(&mut sess, &opts));
    if args.verbose {
        eprint!("{}", sess.render_timings());
    }
    if args.json {
        let failure = match &result {
            Err(BuildError::Failed(msg)) => Some(msg.as_str()),
            _ => None,
        };
        println!("{}", report_json(&sess, failure));
    } else {
        super::build::report(&sess, false);
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(BuildError::Failed(_)) if args.json => ExitCode::from(1),
        Err(e) => super::build::failure_code(&e),
    }
}

/// The file (inside its package, if any) or the current package's root module, resolved like
/// `velt build` (package dependencies are installed if needed).
fn resolve(args: &CheckArgs) -> Result<driver::BuildOptions, BuildError> {
    let input = match &args.input {
        Some(file) => {
            super::project::check_input_file(file).map_err(BuildError::Failed)?;
            file.clone()
        }
        None => package_root_module().map_err(BuildError::Failed)?,
    };
    let build = BuildArgs {
        input: Some(input),
        locked: args.locked,
        // No backend is resolved for IR output: checking needs neither clang nor a linker.
        emit: Emit::Vir,
        ..Default::default()
    };
    super::build::build_options(&build).map_err(BuildError::Failed)
}

/// The module `velt check` checks in the current package: its runnable entry (`package.entry`,
/// default `src/main.vlt`), or `src/lib.vlt` for a library package (one without a configured
/// entry and without `src/main.vlt`). A configured entry that is missing is an error, as for
/// `velt build`.
fn package_root_module() -> Result<PathBuf, String> {
    let root = Project::current_root()?;
    let manifest = vpm::Manifest::from_dir(&root)?;
    let entry = root.join(&manifest.package.entry);
    let lib = root.join(vpm::manifest::LIB_ENTRY);
    let default_entry = manifest.package.entry == vpm::manifest::DEFAULT_ENTRY;
    if entry.is_file() {
        Ok(entry)
    } else if !default_entry {
        Err(format!(
            "package `{}` has no `{}` to check",
            manifest.package.name, manifest.package.entry
        ))
    } else if lib.is_file() {
        Ok(lib)
    } else {
        Err(format!(
            "package `{}` has neither `{}` nor `{}` to check",
            manifest.package.name,
            manifest.package.entry,
            vpm::manifest::LIB_ENTRY
        ))
    }
}

/// `{"diagnostics": [...], "errors": n, "warnings": n}`; a non-source failure (unreadable root,
/// broken package) becomes an error diagnostic without a location.
fn report_json(sess: &Session, failure: Option<&str>) -> Value {
    let mut diagnostics: Vec<Value> = sess
        .diagnostics
        .iter()
        .map(|d| diagnostic_json(d, &sess.sm))
        .collect();
    diagnostics.extend(failure.map(|msg| {
        json!({"severity": "error", "message": msg, "location": null, "labels": [], "notes": []})
    }));
    let count = |sev: &str| diagnostics.iter().filter(|d| d["severity"] == sev).count();
    let (errors, warnings) = (count("error"), count("warning"));
    json!({"diagnostics": diagnostics, "errors": errors, "warnings": warnings})
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
        let v = report_json(&sess, Some("cannot read b.vlt"));
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
