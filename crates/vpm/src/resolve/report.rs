//! Human-readable resolution failures: every conflicting requirement is shown with the chain of
//! packages that introduced it, so the user can see which dependency to change.

use std::path::Path;

use super::{Requirement, Selected, Source, Want};

/// `req` cannot be satisfied by the already-selected package `sel`.
pub(super) fn conflict(req: &Requirement, sel: &Selected) -> String {
    let mut out = format!(
        "cannot resolve dependencies: conflicting requirements for `{}` (selected {})",
        req.name,
        describe_selected(sel)
    );
    for r in sel.by.iter().chain([req]) {
        out.push_str(&format!(
            "\n  {} requires {}",
            r.chain.join(" → "),
            describe_want(&r.name, &r.want)
        ));
    }
    out
}

/// Nothing matches `req`. `available` lists the published versions if the package exists.
pub(super) fn no_match(req: &Requirement, available: Option<&str>, registry: &Path) -> String {
    let by = req.chain.join(" → ");
    match (&req.want, available) {
        (Want::Registry(_), None) => format!(
            "package `{}` is not in the registry `{}` (required by {by})",
            req.name,
            registry.display()
        ),
        (Want::Registry(_), Some(versions)) => format!(
            "no version of `{}` matches {} (required by {by}); available: {versions}",
            req.name,
            describe_want(&req.name, &req.want)
        ),
        (Want::Path { .. }, _) => format!(
            "the package at the path does not match {} (required by {by})",
            describe_want(&req.name, &req.want)
        ),
    }
}

fn describe_want(name: &str, want: &Want) -> String {
    match want {
        Want::Registry(req) => format!("`{name} {req}`"),
        Want::Path {
            dir,
            req: Some(req),
        } => format!("`{name} {req}` at path `{}`", dir.display()),
        Want::Path { dir, req: None } => format!("`{name}` at path `{}`", dir.display()),
    }
}

fn describe_selected(sel: &Selected) -> String {
    match &sel.pkg.source {
        Source::Registry { .. } => sel.pkg.version.to_string(),
        Source::Path { dir } => format!("{} at path `{}`", sel.pkg.version, dir.display()),
    }
}
