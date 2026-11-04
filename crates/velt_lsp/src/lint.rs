//! Live lint findings for the document (docs/internals/design/lint-profiles.md): the
//! TypeScript-compatibility findings in a package's `tsCompat` folders (`velt check --ts-compat`,
//! docs/internals/design/tsx.md "Sharing components with the client"), and the `perf/…` rules
//! on the syntax tree and types in every file of the package, at the levels its `lint` sets
//! (a hint by default). The rules on the optimized program run only in `velt check --perf`.
//!
//! The lint reuses the document's analysis: the loaded modules, the checker's diagnostics and
//! its IDE analysis, which the rules on types ask, so nothing is parsed or checked twice, and only
//! the document is linted. Like the command, it leaves a document with errors of its own alone
//! (the rules only see valid Velt). For ts-compat, the files in scope are the loaded ones the
//! command would find under the folders ([`vpm::sources::in_folder`]: not under `node_modules/`,
//! `target/`, hidden or symlinked directories or nested packages): an import of any other file
//! leaves the set. The folders and levels come from the package's `package.vlt`, its unsaved
//! text when it is open, so editing it updates the open documents; they are cached
//! ([`FolderCache`]) by the manifest's text or modification time.
//!
//! Each finding's fixes become quick fixes ([`fixes`]); those that keep the behaviour are
//! preferred, so "fix all" applies only them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lsp_types::{DiagnosticSeverity, NumberOrString};
use velt_common::{Applicability, Diagnostic, FileId, Severity};
use velt_lint::{Levels, Profiles};
use velt_tscompat::canonical;
use vpm::manifest::LintConfig;

use crate::analysis::Analysis;
use crate::code_actions::Fix;
use crate::line_index::LineIndex;

/// The `source` of the ts-compat findings' diagnostics, which tells them from the compiler's.
pub const SOURCE: &str = "velt ts-compat";

/// The `source` of the `perf/…` findings' diagnostics.
pub const PERF_SOURCE: &str = "velt perf";

/// The lint findings in the document of `analysis` (at `path`). `overlay` holds the open
/// documents' text.
pub fn findings(
    analysis: &Analysis,
    path: &Path,
    overlay: &HashMap<PathBuf, String>,
    cache: &mut FolderCache,
) -> Vec<Diagnostic> {
    let Some(package) = cache.package_of(path, overlay) else {
        return vec![];
    };
    let file = analysis.file();
    let mut diags = vec![];
    let in_folders = |p: &Path| {
        package
            .folders
            .iter()
            .any(|f| vpm::sources::in_folder(f, p))
    };
    let levels = Levels::new(package.lint.as_ref());
    if in_folders(&canonical(path)) {
        let found = velt_tscompat::lint_program(
            &analysis.modules,
            &analysis.sm,
            &analysis.diagnostics,
            analysis.ide.as_ref(),
            &in_folders,
            &|f| f == file,
        );
        diags.extend(
            found
                .iter()
                .filter_map(|f| velt_lint::ts_compat(f, &levels)),
        );
    }
    if let Some(ide) = analysis.ide.as_ref() {
        let profiles = Profiles::new(Some(package.root.clone()), package.folders.clone());
        diags.extend(velt_lint::perf::lint(
            &analysis.modules,
            &analysis.sm,
            &analysis.diagnostics,
            ide,
            &profiles,
            &levels,
            &|f| f == file,
            false,
        ));
    }
    // The rules on the optimized program don't run here: an allow of one is not unused.
    let ran = |code: &str| {
        code.starts_with("ts-compat/")
            || velt_lint::rules::rule(code).is_some_and(|r| r.kind == velt_lint::Kind::Pattern)
    };
    velt_lint::finish(diags, &analysis.sm, &[file], &ran)
}

/// What the cache keeps of a package: its root, its `tsCompat` folders (canonical) and `lint`.
#[derive(Clone)]
pub struct PackageLint {
    root: PathBuf,
    folders: Vec<PathBuf>,
    lint: Option<LintConfig>,
}

/// The lint settings of the packages seen so far, by manifest path, so an analysis does not
/// read and parse `package.vlt` again. An entry holds while the manifest's text (when it is
/// open) or modification time (when it is not) is the same; the server clears the cache when
/// folders appear or disappear on disk, which changes what the folders' paths resolve to.
#[derive(Default)]
pub struct FolderCache {
    entries: HashMap<PathBuf, (Stamp, PackageLint)>,
}

/// What a cached manifest was read from.
#[derive(PartialEq)]
enum Stamp {
    /// The open document's text.
    Open(String),
    /// The file on disk, last modified then (`None`: unknown, or no file).
    Disk(Option<SystemTime>),
}

impl FolderCache {
    /// Forget every package's settings.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The lint settings of the package around `path`, if it is in one.
    fn package_of(
        &mut self,
        path: &Path,
        overlay: &HashMap<PathBuf, String>,
    ) -> Option<PackageLint> {
        let root = vpm::manifest::find_package_root(path.parent()?)?;
        let manifest_path = root.join(vpm::manifest::MANIFEST_FILE);
        let stamp = match overlay.get(&manifest_path) {
            Some(text) => Stamp::Open(text.clone()),
            None => Stamp::Disk(
                std::fs::metadata(&manifest_path)
                    .and_then(|m| m.modified())
                    .ok(),
            ),
        };
        let fresh = self
            .entries
            .get(&manifest_path)
            .is_some_and(|(seen, _)| *seen == stamp);
        if !fresh {
            let manifest = match &stamp {
                Stamp::Open(text) => vpm::Manifest::read(FileId(0), text).ok(),
                Stamp::Disk(_) => vpm::Manifest::from_path(&manifest_path).ok(),
            };
            let folders = manifest.as_ref().map_or_else(Vec::new, |m| {
                m.ts_compat_dirs(&root)
                    .iter()
                    .map(|d| canonical(d))
                    .collect()
            });
            let package = PackageLint {
                root: canonical(&root),
                folders,
                lint: manifest.and_then(|m| m.lint),
            };
            self.entries.insert(manifest_path.clone(), (stamp, package));
        }
        self.entries.get(&manifest_path).map(|(_, p)| p.clone())
    }
}

/// `d` as an LSP diagnostic: `velt <family>` as its source and the rule as its code (as the
/// last note shows it, `family(rule)`), its notes after the message. A `perf/…` hint is a hint.
pub fn diagnostic(index: &LineIndex, d: &Diagnostic) -> lsp_types::Diagnostic {
    let mut message = d.message.clone();
    for note in &d.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    let code = d.code.as_deref().unwrap_or("");
    let (family, rule) = code.split_once('/').unwrap_or(("", code));
    let source = match family {
        "ts-compat" => SOURCE.to_string(),
        "perf" => PERF_SOURCE.to_string(),
        other => format!("velt {other}"),
    };
    let range = d
        .labels
        .first()
        .map_or(index.range(0, 0), |l| index.range(l.span.lo, l.span.hi));
    lsp_types::Diagnostic {
        range,
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
            Severity::Note => DiagnosticSeverity::HINT,
        }),
        code: (!rule.is_empty()).then(|| NumberOrString::String(rule.to_string())),
        source: Some(source),
        message,
        ..Default::default()
    }
}

/// The fixes of the lint findings and compiler diagnostics in the document's byte range
/// `lo..hi`, each a quick fix tied to its diagnostic; preferred when it keeps the behaviour.
pub fn fixes(analysis: &Analysis, lo: u32, hi: u32) -> Vec<Fix> {
    let file = analysis.file();
    let in_range = |d: &&Diagnostic| {
        d.labels
            .first()
            .is_some_and(|l| l.span.file == file && l.span.hi >= lo && l.span.lo <= hi)
    };
    analysis
        .lint
        .iter()
        .chain(&analysis.diagnostics)
        .filter(in_range)
        .flat_map(|d| {
            d.fixes.iter().map(move |fix| Fix {
                title: fix.title.clone(),
                edits: fix.edits.clone(),
                // Matched to the client's diagnostic by range and message.
                diagnostic: Some(Diagnostic::error(d.message.clone(), d.labels[0].span)),
                preferred: fix.applicability == Applicability::Safe,
            })
        })
        .collect()
}
