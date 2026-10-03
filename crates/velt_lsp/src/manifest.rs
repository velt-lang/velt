//! `package.vlt`, the package manifest, is not analyzed as a program: `velt` reads it as data and
//! never compiles it (docs/internals/design/package-manifest.md "Editors"). Its diagnostics are
//! the reader's own (`vpm::manifest::read`, what every `velt` command reports), and completion and
//! hover come from the manifest's field schema (`vpm::manifest::ide`). The registry adds versions
//! and package names to completion, checks the requirements and explains each dependency
//! ([`crate::registry`] fetches in the background; until the data is there it says nothing).

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CompletionItem, CompletionItemKind,
    CompletionList, CompletionTextEdit, Documentation, Hover, HoverContents, InsertTextFormat,
    MarkupContent, MarkupKind, TextEdit, Url, WorkspaceEdit,
};
use velt_common::{Diagnostic, FileId, Severity, Span};
use vpm::manifest::ide::{self, registry as reg};
use vpm::registry::Index;

use crate::line_index::LineIndex;
use crate::registry::{locations_for, Lookup, RegistryData};

/// Whether `path` is a package manifest.
pub fn is_manifest(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n == vpm::manifest::MANIFEST_FILE)
}

/// The diagnostics for manifest `text` (in directory `dir`, for its `velt.lock.json` and
/// `tsCompat` folders): the reader's, warnings for `tsCompat` folders that are not there, then
/// the registry's for the dependencies whose data has arrived.
pub fn diagnostics(
    text: &str,
    data: &RegistryData,
    dir: Option<&Path>,
) -> Vec<lsp_types::Diagnostic> {
    let mut diags = vpm::Manifest::read(FileId(0), text)
        .err()
        .unwrap_or_default();
    if let Some(dir) = dir {
        // The folders `velt check --ts-compat` would fail on.
        diags.extend(vpm::manifest::read::missing_ts_compat_dirs(
            FileId(0),
            text,
            dir,
        ));
    }
    if let Some(loc) = locations_for(text) {
        let registry = loc.describe();
        let lock = lockfile(dir);
        diags.extend(reg::diagnostics(
            text,
            &registry,
            &mut known_index(data, &loc),
            &|name| locked(lock.as_ref(), name),
        ));
    }
    let index = LineIndex::new(text);
    diags.iter().map(|d| convert(&index, d)).collect()
}

fn convert(index: &LineIndex, d: &Diagnostic) -> lsp_types::Diagnostic {
    let span = d.labels.first().map_or(Span::DUMMY, |l| l.span);
    let mut message = d.message.clone();
    for note in &d.notes {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    lsp_types::Diagnostic {
        range: index.range(span.lo, span.hi),
        severity: Some(match d.severity {
            Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
            Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
            Severity::Note => lsp_types::DiagnosticSeverity::INFORMATION,
        }),
        source: Some("velt".into()),
        message,
        ..Default::default()
    }
}

fn lockfile(dir: Option<&Path>) -> Option<vpm::lockfile::Lockfile> {
    vpm::lockfile::Lockfile::read(dir?).ok().flatten()
}

fn locked(lock: Option<&vpm::lockfile::Lockfile>, name: &str) -> Option<String> {
    Some(lock?.get(name)?.version.clone())
}

/// How long completion waits for registry data that is already on its way (editors do not ask
/// again by themselves).
const COMPLETION_WAIT: Duration = Duration::from_millis(400);

/// `name`'s index when it has been fetched (`Some(None)`: not in the registry).
fn known_index<'a>(
    data: &'a RegistryData,
    loc: &'a vpm::Locations,
) -> impl FnMut(&str) -> Option<Option<Index>> + 'a {
    move |name| match data.index(loc, name) {
        Lookup::Ready(index) => Some(index),
        Lookup::Pending | Lookup::Unavailable => None,
    }
}

/// Completions at byte `offset` of manifest `text`: the schema's, plus versions or package names
/// from the registry. Completion waits a little for registry data on its way, and is marked
/// incomplete if it is still missing (the editor asks again on the next keystroke).
pub fn completion(text: &str, offset: u32, data: &RegistryData) -> CompletionList {
    let mut items = ide::completions(text, offset);
    let mut incomplete = false;
    if let (Some(c), Some(loc)) = (reg::cursor(text, offset), locations_for(text)) {
        match &c.ask {
            reg::Ask::Versions { name } => match data
                .settle(COMPLETION_WAIT, || data.index(&loc, name))
            {
                Lookup::Ready(Some(index)) => items.extend(reg::version_completions(&c, &index)),
                Lookup::Pending => incomplete = true,
                Lookup::Ready(None) | Lookup::Unavailable => {}
            },
            reg::Ask::Names { query, .. } => {
                match data.settle(COMPLETION_WAIT, || data.search(&loc, query)) {
                    Lookup::Ready(hits) => items.extend(reg::name_completions(&c, &hits)),
                    Lookup::Pending => incomplete = true,
                    Lookup::Unavailable => {}
                }
            }
        }
    }
    let index = LineIndex::new(text);
    CompletionList {
        is_incomplete: incomplete,
        items: items.into_iter().map(|c| item(&index, c)).collect(),
    }
}

fn item(index: &LineIndex, c: ide::Completion) -> CompletionItem {
    CompletionItem {
        label: c.label,
        kind: Some(match c.kind {
            ide::CompletionKind::Field => CompletionItemKind::FIELD,
            ide::CompletionKind::Value => CompletionItemKind::VALUE,
        }),
        detail: Some(c.detail),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: c.doc,
        })),
        sort_text: Some(c.sort),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(
            index.range(c.replace.start, c.replace.end),
            c.text,
        ))),
        insert_text_format: Some(if c.snippet {
            InsertTextFormat::SNIPPET
        } else {
            InsertTextFormat::PLAIN_TEXT
        }),
        ..Default::default()
    }
}

/// Hover at byte `offset` of manifest `text` (in directory `dir`, for its `velt.lock.json`): a
/// field's documentation, or what the registry says about a dependency.
pub fn hover(text: &str, offset: u32, data: &RegistryData, dir: Option<&Path>) -> Option<Hover> {
    let (markdown, range) = match reg::dependency_at(text, offset) {
        Some(dep) => {
            let loc = locations_for(text)?;
            let index = match data.index(&loc, &dep.name) {
                Lookup::Ready(index) => index,
                Lookup::Pending | Lookup::Unavailable if dep.has_path => None,
                Lookup::Pending | Lookup::Unavailable => return None,
            };
            let locked = locked(lockfile(dir).as_ref(), &dep.name);
            let text = reg::hover(&dep, index.as_ref(), locked.as_deref());
            (text, dep.name_range)
        }
        None => {
            let h = ide::hover(text, offset)?;
            (h.markdown, h.range)
        }
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: Some(LineIndex::new(text).range(range.start, range.end)),
    })
}

/// Quick fixes for bytes `lo..hi` of manifest `text` at `uri` (in directory `dir`): move a
/// requirement to the newest version.
pub fn code_actions(
    uri: &Url,
    text: &str,
    (lo, hi): (u32, u32),
    data: &RegistryData,
    dir: Option<&Path>,
) -> Vec<CodeActionOrCommand> {
    let Some(loc) = locations_for(text) else {
        return vec![];
    };
    let index = LineIndex::new(text);
    let mut lookup = known_index(data, &loc);
    let lock = lockfile(dir);
    let fixes = reg::fixes(text, lo, hi, &mut lookup, &|name| {
        locked(lock.as_ref(), name)
    });
    fixes
        .into_iter()
        .map(|fix| {
            let edit = TextEdit::new(index.range(fix.range.start, fix.range.end), fix.text);
            CodeActionOrCommand::CodeAction(CodeAction {
                title: fix.title,
                kind: Some(CodeActionKind::QUICKFIX),
                edit: Some(WorkspaceEdit::new(HashMap::from([(
                    uri.clone(),
                    vec![edit],
                )]))),
                ..Default::default()
            })
        })
        .collect()
}
