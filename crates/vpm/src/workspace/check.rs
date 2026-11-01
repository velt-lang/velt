//! The checks of [`Workspace::load`] that need the workspace file and every member, each reported
//! where it is written: `velt.workspace.vlt:4:5: error: …`, `apps/b/package.vlt:6:11: error: …`.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use velt_common::{Diagnostic, FileId, SourceMap, Span};

use super::{Member, Workspace, WorkspaceFile, MEMBER_WALK, WORKSPACE_FILE};
use crate::manifest::ide::locate;
use crate::manifest::MANIFEST_FILE;
use crate::npm::workspace::{dirs_with, MAX_DIRS};

/// The problems found so far, rendered.
#[derive(Default)]
pub(super) struct Report {
    errors: Vec<String>,
}

impl Report {
    /// An error already rendered (a member's manifest that does not read).
    pub fn push(&mut self, error: String) {
        self.errors.push(error);
    }

    /// `message` at `range` of the file `shown` (as messages name it) whose text is `src`, or at
    /// the file when the place is unknown.
    fn at(&mut self, shown: &str, src: &str, range: Option<Range<u32>>, message: String) {
        let text = match range {
            Some(r) => {
                let mut sm = SourceMap::new();
                sm.add(Path::new(shown), src);
                Diagnostic::error(message, Span::new(FileId(0), r.start, r.end)).render(&sm)
            }
            None => format!("{shown}: error: {message}"),
        };
        self.errors.push(text);
    }

    pub fn finish(self) -> Result<(), String> {
        match self.errors.is_empty() {
            true => Ok(()),
            false => Err(self.errors.join("\n")),
        }
    }
}

/// Every glob that includes must list a package.
pub(super) fn globs(root: &Path, file: &WorkspaceFile, src: &str, report: &mut Report) {
    let globs = file.members.iter().zip(&file.member_spans);
    for (pattern, span) in globs.filter(|(p, _)| !p.starts_with('!')) {
        let listing = dirs_with(root, std::slice::from_ref(pattern), MEMBER_WALK, MAX_DIRS);
        let message = if listing.cut.is_some() {
            format!("`{pattern}` lists more than {MAX_DIRS} directories; narrow it")
        } else if listing.dirs.is_empty() {
            format!("`{pattern}` matches no package (no directory it names has a {MANIFEST_FILE})")
        } else {
            continue;
        };
        report.at(WORKSPACE_FILE, src, Some(span.clone()), message);
    }
}

/// The checks across members: distinct names, `velt` and `registry` left to the workspace,
/// members depended on with `"workspace"` and every `"workspace"` meaning something, and no cycle
/// between members.
pub(super) fn members(ws: &Workspace, report: &mut Report) {
    let mut names: BTreeMap<&str, &Member> = BTreeMap::new();
    for m in &ws.members {
        match names.entry(m.name()) {
            Entry::Occupied(first) => report.at(
                &file_of(ws, m),
                &m.src,
                locate(&m.src, &[], "name"),
                format!(
                    "two members are named `{}`: {} and this one",
                    m.name(),
                    ws.shown(&first.get().dir)
                ),
            ),
            Entry::Vacant(slot) => {
                slot.insert(m);
            }
        }
    }
    for name in ws.file.dependencies.keys() {
        if names.contains_key(name.as_str()) {
            report.at(
                WORKSPACE_FILE,
                &ws.src,
                locate(&ws.src, &["dependencies"], name),
                format!(
                    "`{name}` is a member of this workspace; members depend on it with `{name}: \"workspace\"`"
                ),
            );
        }
    }
    for m in &ws.members {
        let file = file_of(ws, m);
        for (set, field) in [
            (m.manifest.toolchain.is_some(), "velt"),
            (m.manifest.registry.is_some(), "registry"),
        ] {
            if !set {
                continue;
            }
            let message = match locate(&ws.src, &[], field) {
                Some(r) => format!(
                    "`{field}` is set by the workspace ({WORKSPACE_FILE}:{}); remove it here",
                    line_of(&ws.src, r.start)
                ),
                None => {
                    format!("`{field}` of a member is the workspace's: move it to {WORKSPACE_FILE}")
                }
            };
            report.at(&file, &m.src, locate(&m.src, &[], field), message);
        }
        for (name, dep) in &m.manifest.dependencies {
            let at = || locate(&m.src, &["dependencies"], name);
            if dep.is_workspace() {
                if !names.contains_key(name.as_str()) && !ws.file.dependencies.contains_key(name) {
                    report.at(&file, &m.src, at(), unknown(ws, name, &names));
                }
            } else if names.contains_key(name.as_str()) {
                report.at(
                    &file,
                    &m.src,
                    at(),
                    format!(
                        "`{name}` is a member of this workspace; write `{name}: \"workspace\"`"
                    ),
                );
            }
        }
    }
    cycle(ws, &names, report);
}

fn unknown(ws: &Workspace, name: &str, members: &BTreeMap<&str, &Member>) -> String {
    let mut known = members
        .keys()
        .copied()
        .chain(ws.file.dependencies.keys().map(String::as_str));
    let max = name.chars().count() / 3;
    let close = known.find(|k| crate::manifest::read::edit_distance(k, name) <= max);
    let mut message = format!(
        "`{name}: \"workspace\"`, but no member and no workspace dependency is named `{name}`"
    );
    if let Some(close) = close {
        message.push_str(&format!(" (did you mean `{close}`?)"));
    }
    message
}

/// Members that depend on each other in a cycle can't be built one after another (design step 2
/// runs them in dependency order): the first cycle found, reported at its first dependency.
fn cycle(ws: &Workspace, names: &BTreeMap<&str, &Member>, report: &mut Report) {
    let edges: BTreeMap<&str, Vec<&str>> = names
        .iter()
        .map(|(name, m)| {
            let deps = m.manifest.dependencies.iter();
            let on_members = deps
                .filter(|(n, d)| d.is_workspace() && names.contains_key(n.as_str()))
                .map(|(n, _)| n.as_str())
                .collect();
            (*name, on_members)
        })
        .collect();
    let mut done = BTreeMap::new();
    for start in edges.keys() {
        let mut stack = vec![];
        let Some(found) = visit(start, &edges, &mut done, &mut stack) else {
            continue;
        };
        let m = names[found[0]];
        report.at(
            &file_of(ws, m),
            &m.src,
            locate(&m.src, &["dependencies"], found[1]),
            format!(
                "members depend on each other in a cycle: {}",
                found.join(" → ")
            ),
        );
        return;
    }
}

/// Depth-first from `name`: a cycle through the members on `stack`, as names from its first
/// member back to it. `done` holds the members whose dependencies have no cycle.
fn visit<'a>(
    name: &'a str,
    edges: &BTreeMap<&'a str, Vec<&'a str>>,
    done: &mut BTreeMap<&'a str, ()>,
    stack: &mut Vec<&'a str>,
) -> Option<Vec<&'a str>> {
    if done.contains_key(name) {
        return None;
    }
    if let Some(pos) = stack.iter().position(|s| *s == name) {
        let mut found = stack[pos..].to_vec();
        found.push(name);
        return Some(found);
    }
    stack.push(name);
    for next in &edges[name] {
        if let Some(found) = visit(next, edges, done, stack) {
            return Some(found);
        }
    }
    stack.pop();
    done.insert(name, ());
    None
}

/// The member's manifest as messages name it: relative to the workspace root.
fn file_of(ws: &Workspace, m: &Member) -> String {
    match ws.shown(&m.dir).as_str() {
        "." => MANIFEST_FILE.to_string(),
        dir => format!("{dir}/{MANIFEST_FILE}"),
    }
}

fn line_of(src: &str, offset: u32) -> usize {
    src[..offset as usize].matches('\n').count() + 1
}
