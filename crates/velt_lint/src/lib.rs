//! Lint rules with profiles (docs/internals/design/lint-profiles.md): every source file has a
//! [`Profile`] (portable, native, dependency), every rule a code and a level (`lint` in
//! `package.vlt`), and every fix says whether it stays valid TypeScript, so a portable file is
//! only given fixes that keep it in the common subset, and a dependency none.
//!
//! Two families of rules report through here:
//! - `ts-compat/…`: the TypeScript/Velt common subset (`velt_tscompat`), in portable files;
//! - `perf/…`: patterns that cost speed in Velt ([`perf`]: from the syntax tree and the checker's
//!   types) and what the optimizer could not do ([`evidence`]: from the optimized program).
//!
//! Everything ends as a [`velt_common::Diagnostic`] with its code and fixes, which
//! `velt check` prints (and `--fix` applies) and the language server sends.

mod allow;
pub mod evidence;
pub mod fix;
pub mod perf;
mod profile;
pub mod rules;

use std::path::Path;

use velt_common::{Diagnostic, Fix, Severity, Span};
use vpm::manifest::LintLevel;

pub use allow::Allows;
pub use profile::{Profile, Profiles};
pub use rules::{Kind, Levels, PERF_RULES};

/// A fix with whether it is also valid TypeScript (so a portable file may get it).
#[derive(Clone, Debug)]
pub struct ProfiledFix {
    pub fix: Fix,
    /// The code after the fix is still in the TypeScript/Velt common subset.
    pub portable: bool,
}

/// One `perf/…` finding, before its profile and level decide how it is reported.
#[derive(Clone, Debug)]
pub struct Finding {
    /// The rule (`perf/json-parse-cast`), one of [`PERF_RULES`].
    pub code: &'static str,
    pub span: Span,
    /// What costs speed, in one line.
    pub message: String,
    /// Why, and what to write instead.
    pub notes: Vec<String>,
    /// The fixes, the preferred one first.
    pub fixes: Vec<ProfiledFix>,
}

/// `finding` in a file of `profile` (whose edits' files have the profiles `profile_of` gives) at
/// the package's levels, as a diagnostic: `None` when its level is `off` or the file is a
/// dependency's. In a portable file only portable fixes are kept, and a finding left without one
/// is a note whatever its level: its author can't act on it without leaving the subset.
pub fn report(
    finding: Finding,
    profile: Profile,
    profile_of: &dyn Fn(Span) -> Profile,
    levels: &Levels,
) -> Option<Diagnostic> {
    if profile == Profile::Dependency {
        return None;
    }
    let mut severity = rules::severity(levels.perf(finding.code))?;
    let had_fixes = !finding.fixes.is_empty();
    let fixes: Vec<Fix> = finding
        .fixes
        .into_iter()
        .filter(|f| {
            f.fix
                .edits
                .iter()
                .all(|(s, _)| profile_of(*s).allows(f.portable))
        })
        .map(|f| f.fix)
        .collect();
    let mut notes = finding.notes;
    if profile == Profile::Portable && fixes.is_empty() {
        severity = Severity::Note;
        if had_fixes {
            notes.push(
                "this file is shared with TypeScript (`tsCompat`), and the fix is Velt-only".into(),
            );
        }
    }
    notes.extend(fixes.first().map(fix_note));
    notes.push(rules::code_note(finding.code));
    Some(Diagnostic {
        severity,
        message: finding.message,
        labels: Diagnostic::error("", finding.span).labels,
        notes,
        code: Some(finding.code.into()),
        fixes,
    })
}

/// The note that says what a finding's first fix does and which `--fix` applies it.
fn fix_note(fix: &Fix) -> String {
    let flag = match fix.applicability {
        velt_common::Applicability::Safe => "--fix",
        velt_common::Applicability::Review => "--fix=review",
    };
    format!("fix: {} (`velt check {flag}`)", fix.title)
}

/// A `velt_tscompat` finding as a diagnostic: its notes, then `ts-compat(<rule>)`, its code
/// `ts-compat/<rule>`, its fix (safe: the rules only offer fixes that keep the behaviour). A
/// level in `lint.rules` replaces its severity; `off` drops it.
pub fn ts_compat(f: &velt_tscompat::Finding, levels: &Levels) -> Option<Diagnostic> {
    let code = rules::ts_compat_code(f.code);
    let severity = match levels.ts_compat(&code) {
        Some(level) => rules::severity(level)?,
        None if f.severity == velt_tscompat::Severity::Warning => Severity::Warning,
        None => Severity::Error,
    };
    let mut d = Diagnostic::error(f.message.clone(), f.span).with_code(code);
    d.severity = severity;
    d.notes = f.notes.clone();
    d.notes.push(format!("ts-compat({})", f.code));
    if let Some(fix) = &f.fix {
        d.notes.insert(
            d.notes.len() - 1,
            format!("fix: {} (`velt check --ts-compat --fix`)", fix.title),
        );
        d.fixes.push(Fix::replace(
            fix.title.clone(),
            fix.span,
            fix.replacement.clone(),
            velt_common::Applicability::Safe,
        ));
    }
    Some(d)
}

/// The lint diagnostics of every family, each file's `velt-allow` comments applied (plus the
/// warnings about those comments), ordered by file and position. `ran` says which codes this
/// run's rules cover, so an allow of a rule that did not run is not reported as unused.
pub fn finish(
    diagnostics: Vec<Diagnostic>,
    sm: &velt_common::SourceMap,
    linted: &[velt_common::FileId],
    ran: &dyn Fn(&str) -> bool,
) -> Vec<Diagnostic> {
    let mut by_file: std::collections::BTreeMap<velt_common::FileId, Vec<Diagnostic>> =
        linted.iter().map(|f| (*f, vec![])).collect();
    let mut out = vec![];
    for d in diagnostics {
        match d.labels.first() {
            Some(l) => by_file.entry(l.span.file).or_default().push(d),
            None => out.push(d),
        }
    }
    for (file, diags) in by_file {
        let Some((_, f)) = sm.files().nth(file.0 as usize) else {
            out.extend(diags);
            continue;
        };
        let src = f.src.as_str();
        let mut allows = Allows::parse(file, src);
        out.extend(diags.into_iter().filter(|d| {
            let (Some(code), Some(label)) = (&d.code, d.labels.first()) else {
                return true;
            };
            !allows.suppresses(code, label.span)
        }));
        out.extend(allows.finish(ran));
    }
    out.sort_by_key(|d| {
        d.labels
            .first()
            .map(|l| (l.span.file, l.span.lo, l.span.hi))
    });
    out
}

/// The warning for each code in `lint.rules` of the manifest at `manifest` that names no rule.
pub fn unknown_codes(levels: &Levels, manifest: &Path) -> Vec<String> {
    levels
        .unknown_codes()
        .into_iter()
        .map(|code| {
            format!(
                "{}: `lint.rules` names `{code}`, which is not a lint rule",
                manifest.display()
            )
        })
        .collect()
}

/// Whether `level` makes plain `velt check` (without `--perf`) run the rules of `kind`.
pub fn runs_without_perf(levels: &Levels, kind: Kind) -> bool {
    kind == Kind::Pattern && levels.any_perf(kind, LintLevel::Warn)
}
