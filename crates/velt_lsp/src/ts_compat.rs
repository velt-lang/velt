//! Live `velt check --ts-compat` findings for documents in a package's `tsCompat` folders
//! (docs/internals/design/tsx.md "Sharing components with the client").
//!
//! The lint reuses the document's analysis: the loaded modules, the checker's diagnostics and
//! its IDE analysis, which the rules on types ask ([`velt_tscompat::lint_program`]), so nothing
//! is parsed or checked twice, and only the
//! document is linted. Like the command, it leaves a document with errors of its own alone (the
//! rules only see valid Velt), and the files in scope are the loaded ones the command would find
//! under the folders ([`vpm::sources::in_folder`]: not under `node_modules/`, `target/`, hidden
//! or symlinked directories or nested packages): an import of any other file leaves the set.
//! The folders come from the package's `package.vlt`, its unsaved text when it is open, so
//! editing `tsCompat` updates the open documents; they are cached ([`FolderCache`]) by the
//! manifest's text or modification time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lsp_types::{DiagnosticSeverity, NumberOrString};
use velt_common::{Diagnostic, FileId};
use velt_tscompat::{canonical, Finding, Severity};

use crate::analysis::Analysis;
use crate::code_actions::Fix;
use crate::line_index::LineIndex;

/// The `source` of the findings' diagnostics, which tells them from the compiler's.
pub const SOURCE: &str = "velt ts-compat";

/// The findings in the document of `analysis` (at `path`), or none when it is not in a
/// `tsCompat` folder of its package. `overlay` holds the open documents' text.
pub fn findings(
    analysis: &Analysis,
    path: &Path,
    overlay: &HashMap<PathBuf, String>,
    cache: &mut FolderCache,
) -> Vec<Finding> {
    let Some(folders) = cache.folders_of(path, overlay) else {
        return vec![];
    };
    let in_folders = |p: &Path| folders.iter().any(|f| vpm::sources::in_folder(f, p));
    if !in_folders(&canonical(path)) {
        return vec![];
    }
    let file = analysis.file();
    velt_tscompat::lint_program(
        &analysis.modules,
        &analysis.sm,
        &analysis.diagnostics,
        analysis.ide.as_ref(),
        &in_folders,
        &|f| f == file,
    )
}

/// The `tsCompat` folders of the packages seen so far, by manifest path, so an analysis does not
/// read and parse `package.vlt` again. An entry holds while the manifest's text (when it is
/// open) or modification time (when it is not) is the same; the server clears the cache when
/// folders appear or disappear on disk, which changes what the folders' paths resolve to.
#[derive(Default)]
pub struct FolderCache {
    entries: HashMap<PathBuf, (Stamp, Vec<PathBuf>)>,
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
    /// Forget every package's folders.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The canonical `tsCompat` folders of the package around `path`, if it has any.
    fn folders_of(
        &mut self,
        path: &Path,
        overlay: &HashMap<PathBuf, String>,
    ) -> Option<&[PathBuf]> {
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
            let folders = manifest.map_or_else(Vec::new, |m| {
                m.ts_compat_dirs(&root)
                    .iter()
                    .map(|d| canonical(d))
                    .collect()
            });
            self.entries.insert(manifest_path.clone(), (stamp, folders));
        }
        let folders = &self.entries.get(&manifest_path)?.1;
        (!folders.is_empty()).then_some(folders.as_slice())
    }
}

/// `f` as an LSP diagnostic: its rule as the code, its notes after the message.
pub fn diagnostic(index: &LineIndex, f: &Finding) -> lsp_types::Diagnostic {
    let mut message = f.message.clone();
    for note in &f.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    lsp_types::Diagnostic {
        range: index.range(f.span.lo, f.span.hi),
        severity: Some(match f.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        code: Some(NumberOrString::String(f.code.to_string())),
        source: Some(SOURCE.into()),
        message,
        ..Default::default()
    }
}

/// Each finding's fix in the document's byte range `lo..hi`, as a preferred quick fix tied to the
/// finding's diagnostic.
pub fn fixes(analysis: &Analysis, lo: u32, hi: u32) -> Vec<Fix> {
    analysis
        .ts_compat
        .iter()
        .filter(|f| f.span.hi >= lo && f.span.lo <= hi)
        .filter_map(|f| {
            let fix = f.fix.as_ref()?;
            Some(Fix {
                title: fix.title.clone(),
                edits: vec![(fix.span, fix.replacement.clone())],
                // Matched to the client's diagnostic by range and message.
                diagnostic: Some(Diagnostic::error(f.message.clone(), f.span)),
                preferred: true,
            })
        })
        .collect()
}
