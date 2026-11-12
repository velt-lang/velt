//! Human-readable resolution failures: every conflicting requirement is shown with the chain of
//! packages that introduced it, so the user can see which dependency to change.

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

/// `req` names another source than the one selected for its package, and the root's own entry
/// doesn't settle it: two of the root's entries disagree, or two transitive ones do.
pub(super) fn two_sources(req: &Requirement, sel: &Selected) -> String {
    let fix = match req.root {
        true => "make them name the same source".to_string(),
        false => format!(
            "name `{0}` in this package's dependencies to choose one: the root's own entry wins",
            req.name
        ),
    };
    let mut out = format!(
        "cannot resolve dependencies: `{}` is asked for from two sources ({fix})",
        req.name
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

/// The root's entry for `req`'s package (selected in `sel`) has a version `req` doesn't accept:
/// overriding a source never breaks a version requirement.
pub(super) fn overridden_version(req: &Requirement, sel: &Selected) -> String {
    let mut out = format!(
        "cannot resolve dependencies: this package's entry for `{}` gives {}, which {} does not accept",
        req.name,
        describe_selected(sel),
        req.chain.last().map_or("its dependent", String::as_str),
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

/// Nothing matches `req`. `available` lists the published versions if the package exists;
/// `registry` is the registry asked ([`crate::Locations::describe`]: a URL or a directory).
pub(super) fn no_match(req: &Requirement, available: Option<&str>, registry: &str) -> String {
    let by = req.chain.join(" → ");
    match (&req.want, available) {
        (Want::Registry(_), None) => format!(
            "package `{}` is not in the registry `{registry}` (required by {by})",
            req.name,
        ),
        (Want::Registry(_), Some(versions)) => format!(
            "no version of `{}` matches {} (required by {by}); available: {versions}",
            req.name,
            describe_want(&req.name, &req.want)
        ),
        (Want::Path { .. } | Want::Git { .. } | Want::Url { .. }, _) => format!(
            "the package there does not match {} (required by {by})",
            describe_want(&req.name, &req.want)
        ),
    }
}

pub(super) fn describe_want(name: &str, want: &Want) -> String {
    match want {
        Want::Registry(req) => format!("`{name} {req}`"),
        Want::Path {
            dir,
            req: Some(req),
        } => format!("`{name} {req}` at path `{}`", dir.display()),
        Want::Path { dir, req: None } => format!("`{name}` at path `{}`", dir.display()),
        Want::Git {
            spec,
            req: Some(req),
        } => format!("`{name} {req}` from {}", spec.describe()),
        Want::Git { spec, req: None } => format!("`{name}` from {}", spec.describe()),
        Want::Url {
            spec,
            req: Some(req),
        } => format!("`{name} {req}` from {}", spec.describe()),
        Want::Url { spec, req: None } => format!("`{name}` from {}", spec.describe()),
    }
}

fn describe_selected(sel: &Selected) -> String {
    match &sel.pkg.source {
        Source::Registry { .. } => sel.pkg.version.to_string(),
        Source::Path { dir } => format!("{} at path `{}`", sel.pkg.version, dir.display()),
        Source::Git { spec, commit, .. } => format!(
            "{} from {} at commit {}",
            sel.pkg.version,
            spec.describe(),
            &commit[..12]
        ),
        Source::Url { spec, .. } => format!("{} from {}", sel.pkg.version, spec.describe()),
    }
}
