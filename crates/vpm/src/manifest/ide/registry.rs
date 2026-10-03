//! Registry data in the manifest editor (docs/internals/design/package-manifest.md "Editors",
//! live registry data): which dependency the cursor is on, and the completions, diagnostics, hover
//! text and fixes that a registry's index and search give for it.
//!
//! Nothing here fetches: the caller looks the data up (the language server does it off the
//! request thread and caches it) and passes it in, or says it does not have it yet.

use std::ops::Range;

use velt_common::{Diagnostic, FileId, Span};

use super::{scan, At, Completion, CompletionKind};
use crate::registry::{Index, IndexEntry};
use crate::search::Hit;

/// Most exact versions offered by version completion.
const MAX_VERSIONS: usize = 20;

/// A dependency written in `dependencies`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyEntry {
    pub name: String,
    /// The key's bytes (with quotes, if quoted).
    pub name_range: Range<u32>,
    /// The version requirement and the bytes of its contents (inside the quotes), when written.
    pub version: Option<(String, Range<u32>)>,
    /// It has a `path` (a local package: the registry is not asked).
    pub has_path: bool,
}

/// The dependencies of manifest text `src`, in order.
pub fn dependencies(src: &str) -> Vec<DependencyEntry> {
    let scan = scan(src, u32::MAX);
    let Some(deps) = scan
        .frames
        .iter()
        .find(|f| f.object && f.path == ["dependencies"])
    else {
        return vec![];
    };
    deps.keys
        .iter()
        .map(|(name, name_range)| {
            let detail = scan.frames.iter().find(|f| {
                f.object && f.path.len() == 2 && f.path[0] == "dependencies" && f.path[1] == *name
            });
            let version = deps
                .values
                .iter()
                .find(|(k, ..)| k == name)
                .or_else(|| detail.and_then(|f| f.values.iter().find(|(k, ..)| k == "version")))
                .map(|(_, v, range)| (v.clone(), range.clone()));
            let has_path = detail.is_some_and(|f| f.keys.iter().any(|(k, _)| k == "path"));
            DependencyEntry {
                name: name.clone(),
                name_range: name_range.clone(),
                version,
                has_path,
            }
        })
        .collect()
}

/// A top-level string field of manifest text `src` (such as `registry`), even when the rest of
/// the file is not valid.
pub fn top_level_string(src: &str, key: &str) -> Option<String> {
    let scan = scan(src, u32::MAX);
    let root = scan.frames.first()?;
    root.values
        .iter()
        .find(|(k, ..)| k == key)
        .map(|(_, v, _)| v.clone())
}

/// What the registry is asked at the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// The versions of a package: the cursor is on a dependency's requirement.
    Versions { name: String },
    /// Packages whose name contains `query`: the cursor is on a new key in `dependencies`.
    Names { query: String, taken: Vec<String> },
}

/// Where registry completion applies at byte `offset`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryCursor {
    pub ask: Ask,
    /// The bytes a completion replaces.
    pub replace: Range<u32>,
    /// Inside a string literal (`replace` is its contents).
    pub quoted: bool,
}

/// The registry question at byte `offset` of `src`, if the cursor is on one.
pub fn cursor(src: &str, offset: u32) -> Option<RegistryCursor> {
    let scan = scan(src, offset);
    let c = scan.cursor?;
    let frame = &scan.frames[c.frame];
    let ask = match (&c.at, frame.path.as_slice()) {
        (At::Value(name), [deps]) if deps == "dependencies" => Ask::Versions { name: name.clone() },
        (At::Value(key), [deps, name]) if deps == "dependencies" && key == "version" => {
            Ask::Versions { name: name.clone() }
        }
        (At::Key, [deps]) if deps == "dependencies" => Ask::Names {
            query: src[c.replace.start as usize..c.replace.end as usize].to_string(),
            taken: frame
                .keys
                .iter()
                .filter(|(_, r)| Some(r.start) != c.token)
                .map(|(k, _)| k.clone())
                .collect(),
        },
        _ => return None,
    };
    Some(RegistryCursor {
        ask,
        replace: c.replace,
        quoted: c.quoted,
    })
}

/// The published versions of `index` that are not yanked, newest first.
fn available(index: &Index) -> Vec<(semver::Version, &IndexEntry)> {
    let mut out: Vec<_> = index
        .versions
        .iter()
        .filter(|e| !e.yanked)
        .filter_map(|e| Some((semver::Version::parse(&e.version).ok()?, e)))
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// The newest stable version that is not yanked (a pre-release only if there is nothing else).
pub fn newest(index: &Index) -> Option<semver::Version> {
    let all = available(index);
    all.iter()
        .find(|(v, _)| v.pre.is_empty())
        .or(all.first())
        .map(|(v, _)| v.clone())
}

/// Version completions for `c` (an [`Ask::Versions`]): `^<newest>`, then every version, newest
/// first.
pub fn version_completions(c: &RegistryCursor, index: &Index) -> Vec<Completion> {
    let mut texts = vec![];
    if let Some(v) = newest(index) {
        texts.push((
            format!("^{v}"),
            "the newest version, and compatible updates".to_string(),
        ));
    }
    for (v, entry) in available(index).into_iter().take(MAX_VERSIONS) {
        let native = if entry.native.is_empty() {
            ""
        } else {
            " (runs native code)"
        };
        texts.push((v.to_string(), format!("exactly {v}{native}")));
    }
    texts
        .into_iter()
        .enumerate()
        .map(|(i, (label, doc))| Completion {
            text: if c.quoted {
                label.clone()
            } else {
                format!("\"{label}\"")
            },
            label,
            kind: CompletionKind::Value,
            detail: "version".into(),
            doc,
            replace: c.replace.clone(),
            snippet: false,
            sort: format!("{i:04}"),
        })
        .collect()
}

/// Package-name completions for `c` (an [`Ask::Names`]) from search `hits`. Outside quotes a
/// completion writes the whole entry: `name: "^<version>"`.
pub fn name_completions(c: &RegistryCursor, hits: &[Hit]) -> Vec<Completion> {
    let Ask::Names { taken, .. } = &c.ask else {
        return vec![];
    };
    hits.iter()
        .filter(|h| !taken.contains(&h.name))
        .enumerate()
        .map(|(i, h)| {
            let key = crate::manifest::write::key(&h.name);
            let stable = semver::Version::parse(&h.version).is_ok_and(|v| v.pre.is_empty());
            let (text, snippet) = match (c.quoted, stable) {
                (true, _) => (h.name.clone(), false),
                (false, true) => (format!("{key}: \"^{}$1\"", h.version), true),
                // Only a pre-release is out: leave the choice to version completion.
                (false, false) => (format!("{key}: \"$1\""), true),
            };
            Completion {
                label: h.name.clone(),
                kind: CompletionKind::Value,
                detail: format!("latest {}", h.version),
                doc: format!("Package `{}`, newest version {}.", h.name, h.version),
                replace: c.replace.clone(),
                text,
                snippet,
                sort: format!("{i:04}"),
            }
        })
        .collect()
}

/// What the registry says about one dependency's requirement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finding {
    /// The registry has no package by this name.
    NotInRegistry,
    /// No published version (not yanked, or yanked but locked) matches; `newest` is the newest
    /// there is.
    NoMatch { newest: Option<semver::Version> },
    /// Versions match, but a newer one (than every match) is out of the requirement's range.
    Newer { newest: semver::Version },
}

/// Check requirement `req` against `index` (`None`: the package is not in the registry).
/// `locked` is the version `velt.lock.json` pins, which counts even when it is yanked (resolution
/// keeps it, too).
pub fn check(req: &str, index: Option<&Index>, locked: Option<&str>) -> Option<Finding> {
    let Some(index) = index else {
        return Some(Finding::NotInRegistry);
    };
    let req = semver::VersionReq::parse(req).ok()?; // the reader reports a bad requirement
    let locked = locked
        .and_then(|l| semver::Version::parse(l).ok())
        .filter(|l| index.versions.iter().any(|e| e.version == l.to_string()));
    let matching = available(index)
        .into_iter()
        .map(|(v, _)| v)
        .chain(locked)
        .filter(|v| req.matches(v))
        .max();
    let newest = newest(index);
    let Some(best) = matching else {
        return Some(Finding::NoMatch { newest });
    };
    let newest = newest?;
    (newest > best && !req.matches(&newest)).then_some(Finding::Newer { newest })
}

/// The registry diagnostics of manifest text `src`. `lookup(name)` is the package's index:
/// `Some(None)` when the registry has no such package, `None` when it is not known (yet, or the
/// registry is unreachable), which leaves the dependency alone. `locked(name)` is the version
/// `velt.lock.json` pins. `registry` names the registry in messages.
pub fn diagnostics(
    src: &str,
    registry: &str,
    lookup: &mut dyn FnMut(&str) -> Option<Option<Index>>,
    locked: &dyn Fn(&str) -> Option<String>,
) -> Vec<Diagnostic> {
    let mut out = vec![];
    for dep in dependencies(src) {
        let Some((req, range)) = dep.version.filter(|_| !dep.has_path) else {
            continue;
        };
        if !crate::manifest::is_valid_package_name(&dep.name) {
            continue; // the reader reports it
        }
        let Some(index) = lookup(&dep.name) else {
            continue;
        };
        let span = |r: &Range<u32>| Span::new(FileId(0), r.start, r.end);
        let d = match check(&req, index.as_ref(), locked(&dep.name).as_deref()) {
            None => continue,
            Some(Finding::NotInRegistry) => Diagnostic::error(
                format!("package `{}` is not in the registry `{registry}`", dep.name),
                span(&dep.name_range),
            ),
            Some(Finding::NoMatch { newest }) => {
                let newest = newest.map_or(String::new(), |v| format!(" (newest: {v})"));
                Diagnostic::error(
                    format!(
                        "no published version of `{}` matches `{req}`{newest}",
                        dep.name
                    ),
                    span(&range),
                )
            }
            Some(Finding::Newer { newest }) => {
                let mut d = Diagnostic::error(
                    format!(
                        "`{}` {newest} is available; `{req}` does not include it",
                        dep.name
                    ),
                    span(&range),
                );
                d.severity = velt_common::Severity::Note;
                d
            }
        };
        out.push(d);
    }
    out
}

/// A fix: replace `range` of the manifest text with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub title: String,
    pub range: Range<u32>,
    pub text: String,
}

/// Fixes for the dependencies whose requirement overlaps bytes `lo..hi`: move a requirement that
/// leaves out the newest version, or matches nothing, to `^<newest>`.
pub fn fixes(
    src: &str,
    lo: u32,
    hi: u32,
    lookup: &mut dyn FnMut(&str) -> Option<Option<Index>>,
    locked: &dyn Fn(&str) -> Option<String>,
) -> Vec<Fix> {
    let mut out = vec![];
    for dep in dependencies(src) {
        let Some((req, range)) = dep.version.filter(|_| !dep.has_path) else {
            continue;
        };
        let touches = range.start <= hi && lo <= range.end
            || dep.name_range.start <= hi && lo <= dep.name_range.end;
        if !touches {
            continue;
        }
        let Some(Some(index)) = lookup(&dep.name) else {
            continue;
        };
        let newest = match check(&req, Some(&index), locked(&dep.name).as_deref()) {
            Some(Finding::Newer { newest })
            | Some(Finding::NoMatch {
                newest: Some(newest),
            }) => newest,
            _ => continue,
        };
        out.push(Fix {
            title: format!("Use `^{newest}` for `{}`", dep.name),
            range,
            text: format!("^{newest}"),
        });
    }
    out
}

/// Hover text for a dependency `entry`: its newest version, whether the requirement includes it,
/// and the locked version (`locked`), if known.
pub fn hover(entry: &DependencyEntry, index: Option<&Index>, locked: Option<&str>) -> String {
    let mut lines = vec![format!("**{}**", entry.name)];
    match (index, entry.has_path) {
        (_, true) => lines.push("local package (`path`)".into()),
        (None, false) => lines.push("not in the registry".into()),
        (Some(index), false) => {
            if let Some(newest) = newest(index) {
                let mut line = format!("newest: {newest}");
                if let Some((req, _)) = &entry.version {
                    if let Ok(r) = semver::VersionReq::parse(req) {
                        let verb = if r.matches(&newest) {
                            "includes"
                        } else {
                            "does not include"
                        };
                        line.push_str(&format!(" (`{req}` {verb} it)"));
                    }
                }
                lines.push(line);
            }
            let native = available(index)
                .first()
                .is_some_and(|(_, e)| !e.native.is_empty());
            if native {
                lines.push("runs native code (prebuilt per target)".into());
            }
        }
    }
    if let Some(locked) = locked {
        lines.push(format!("locked: {locked} (`velt.lock.json`)"));
    }
    lines.join("  \n")
}

/// The dependency whose name is under byte `offset`.
pub fn dependency_at(src: &str, offset: u32) -> Option<DependencyEntry> {
    dependencies(src)
        .into_iter()
        .find(|d| d.name_range.start <= offset && offset <= d.name_range.end)
}

#[cfg(test)]
mod tests;
