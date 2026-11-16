//! EXPERIMENT (#2943 M2, `VELT_UNITS_DISK`): std's files as a snapshot, so a load whose std
//! modules come from std's unit asks the file system nothing about std. The driver makes the
//! snapshot from a walk of the std root ([`StdTree::walk`]) and keeps it with std's unit; a later
//! check reads it back and checks it with one `stat` per directory and file ([`StdTree::fresh`]):
//! a file added, removed or renamed changes its directory's time, an edit its file's length or
//! time. [`install`] makes it the answer to the loader's questions about paths below the std
//! root: whether a file exists ([`is_file`]), a directory's names ([`names`]), a path's identity
//! ([`key`], which `file_key` asks the file system for otherwise) and std's sources, which a
//! load with std's stubs never reads ([`skips_read`]).
//!
//! A tree with a link below the root is not made (a path's identity would not be the root's
//! joined with it). While the build stamp records what resolution looked at
//! (`vpm::probe::record`), the snapshot answers nothing: the recording must see the files.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

/// std's directories and files below its root, with what [`StdTree::fresh`] compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdTree {
    /// The root as the loader is given it (absolute, normalized).
    root: PathBuf,
    /// The root's canonical path ([`key`] of a path below it is this joined with the rest).
    canonical: PathBuf,
    /// Each directory below the root (`""`: the root), with its time of change, by path relative
    /// to the root (`/`-separated).
    dirs: BTreeMap<String, u128>,
    /// Each file below the root, with its length and time of change.
    files: BTreeMap<String, (u64, u128)>,
}

const FORMAT: &str = "velt-std-tree 1";

impl StdTree {
    /// Walk `root`; `None` when it cannot be read whole or holds a link.
    pub fn walk(root: &Path) -> Option<StdTree> {
        let root = vpm::relpath::absolute(root);
        let canonical = std::fs::canonicalize(&root).ok()?;
        let mut tree = StdTree {
            root,
            canonical,
            dirs: BTreeMap::new(),
            files: BTreeMap::new(),
        };
        let mut todo = vec![String::new()];
        while let Some(rel) = todo.pop() {
            let dir = tree.root.join(&rel);
            tree.dirs.insert(rel.clone(), time(&std::fs::metadata(&dir).ok()?));
            for entry in std::fs::read_dir(&dir).ok()? {
                let entry = entry.ok()?;
                let name = entry.file_name().into_string().ok()?;
                let sub = if rel.is_empty() { name } else { format!("{rel}/{name}") };
                let meta = std::fs::symlink_metadata(entry.path()).ok()?;
                if meta.is_dir() {
                    todo.push(sub);
                } else if meta.is_file() {
                    tree.files.insert(sub, (meta.len(), time(&meta)));
                } else {
                    return None;
                }
            }
        }
        Some(tree)
    }

    /// Whether every directory and file is as it was when the tree was made, and nothing was
    /// added or removed.
    pub fn fresh(&self) -> bool {
        let canonical = std::fs::canonicalize(&self.root).ok();
        canonical.as_deref() == Some(self.canonical.as_path())
            && (self.dirs.iter()).all(|(rel, &t)| {
                std::fs::metadata(self.root.join(rel)).is_ok_and(|m| m.is_dir() && time(&m) == t)
            })
            && (self.files.iter()).all(|(rel, &(len, t))| {
                std::fs::symlink_metadata(self.root.join(rel))
                    .is_ok_and(|m| m.is_file() && (m.len(), time(&m)) == (len, t))
            })
    }

    /// The root the tree is of (absolute, normalized).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The files below the root, as paths below [`StdTree::root`], in order.
    pub fn files(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.files.keys().map(|rel| self.root.join(rel))
    }

    /// The tree as text ([`StdTree::decode`] reads it back).
    pub fn encode(&self) -> String {
        let mut out = format!("{FORMAT}\n{}\n{}\n", self.root.display(), self.canonical.display());
        for (rel, t) in &self.dirs {
            out.push_str(&format!("d\t{t}\t{rel}\n"));
        }
        for (rel, (len, t)) in &self.files {
            out.push_str(&format!("f\t{t}\t{len}\t{rel}\n"));
        }
        out
    }

    pub fn decode(text: &str) -> Option<StdTree> {
        let mut lines = text.lines();
        (lines.next()? == FORMAT).then_some(())?;
        let root = PathBuf::from(lines.next()?);
        let canonical = PathBuf::from(lines.next()?);
        let (mut dirs, mut files) = (BTreeMap::new(), BTreeMap::new());
        for line in lines {
            let mut parts = line.splitn(4, '\t');
            match (parts.next()?, parts.next()?.parse().ok()?) {
                ("d", t) => {
                    dirs.insert(parts.next()?.to_string(), t);
                }
                ("f", t) => {
                    let len = parts.next()?.parse().ok()?;
                    files.insert(parts.next()?.to_string(), (len, t));
                }
                _ => return None,
            }
        }
        Some(StdTree {
            root,
            canonical,
            dirs,
            files,
        })
    }

    /// `path` relative to the root (`/`-separated) when it is below it as given or as its
    /// canonical path.
    fn below(&self, path: &Path) -> Option<String> {
        // Most paths the loader asks about are the root joined with plain names.
        if let Some(rel) = plain_below(path, &self.root).or_else(|| plain_below(path, &self.canonical)) {
            return Some(rel.to_string());
        }
        let path = if path.is_absolute() {
            vpm::relpath::normalize(path)
        } else {
            vpm::relpath::absolute(path)
        };
        let rel = (path.strip_prefix(&self.root).ok())
            .or_else(|| path.strip_prefix(&self.canonical).ok())?;
        let mut out = String::new();
        for c in rel.components() {
            let std::path::Component::Normal(name) = c else {
                return None;
            };
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(name.to_str()?);
        }
        Some(out)
    }
}

/// `path` below `root` as a `/`-separated string, when it is `root` joined with plain names
/// (no `.` or `..`): a byte comparison, no normalizing.
fn plain_below<'p>(path: &'p Path, root: &Path) -> Option<&'p str> {
    let rest = path.as_os_str().as_encoded_bytes();
    let rest = rest.strip_prefix(root.as_os_str().as_encoded_bytes())?;
    let rest = match rest {
        [] => "",
        [b'/', rest @ ..] => std::str::from_utf8(rest).ok()?,
        _ => return None,
    };
    let plain = !rest.ends_with('/')
        && (rest.is_empty() || rest.split('/').all(|n| !n.is_empty() && n != "." && n != ".."));
    plain.then_some(rest)
}

/// The installed snapshot ([`install`]) with what it answers precomputed.
struct Installed {
    tree: StdTree,
    stubbed: BTreeSet<String>,
    /// Each directory's names, sorted.
    names: std::collections::HashMap<String, Vec<String>>,
    files: std::collections::HashSet<String>,
}

/// The snapshot the loader answers from ([`install`]).
static TREE: OnceLock<Installed> = OnceLock::new();

/// Answer the loader's questions about std's files from `tree` for the rest of the process.
/// `stubbed`: the canonical paths of the std modules whose stubs the load is given, whose
/// sources it then doesn't read.
pub fn install(tree: StdTree, stubbed: BTreeSet<String>) {
    let mut names: std::collections::HashMap<String, Vec<String>> =
        tree.dirs.keys().map(|d| (d.clone(), vec![])).collect();
    for p in tree.dirs.keys().filter(|d| !d.is_empty()).chain(tree.files.keys()) {
        let (dir, name) = p.rsplit_once('/').unwrap_or(("", p));
        names.entry(dir.to_string()).or_default().push(name.to_string());
    }
    names.values_mut().for_each(|n| n.sort());
    let files = tree.files.keys().cloned().collect();
    let _ = TREE.set(Installed {
        tree,
        stubbed,
        names,
        files,
    });
}

fn installed() -> Option<&'static Installed> {
    (!vpm::probe::recording()).then_some(())?;
    TREE.get()
}

fn tree() -> Option<&'static StdTree> {
    installed().map(|i| &i.tree)
}

/// Whether `path` is a file, when it is below the snapshot's root.
pub(crate) fn is_file(path: &Path) -> Option<bool> {
    let i = installed()?;
    let rel = i.tree.below(path)?;
    Some(i.files.contains(&rel))
}

/// The names in directory `dir`, when it is below the snapshot's root.
pub(crate) fn names(dir: &Path) -> Option<Vec<String>> {
    let i = installed()?;
    let rel = i.tree.below(dir)?;
    Some(i.names.get(&rel).cloned().unwrap_or_default())
}

/// The identity `file_key` gives `path` (its canonical path), when its directory is below the
/// snapshot's root.
pub(crate) fn key(path: &Path) -> Option<PathBuf> {
    let t = tree()?;
    let rel = t.below(path)?;
    let parent = rel.rsplit_once('/').map_or("", |(dir, _)| dir);
    if rel.is_empty() || t.dirs.contains_key(&rel) || t.dirs.contains_key(parent) {
        return Some(t.canonical.join(&rel));
    }
    None
}

/// Whether the load is given the stub of the std module at canonical path `canonical`, so it
/// doesn't read its source.
pub(crate) fn skips_read(canonical: &str) -> bool {
    installed().is_some_and(|i| i.stubbed.contains(canonical))
}

fn time(m: &std::fs::Metadata) -> u128 {
    (m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}
