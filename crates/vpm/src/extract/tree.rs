//! The tree an archive holds, checked entry by entry as the readers add them ([`Tree::add`]) and
//! as a whole once they are done ([`Tree::finish`]): names, clashes, duplicates, paths under
//! links, hard links, and where each symbolic link leads.

use std::collections::{BTreeMap, HashMap};

use super::names::{self, clash_key};
use super::Limits;

/// The most bytes all paths and the directories above them may add up to.
const MAX_NAME_BYTES: usize = 64 << 20;

/// One entry, as an archive gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Entry {
    File(Vec<u8>),
    Dir,
    /// A symbolic link and its target, as written.
    Symlink(String),
    /// A hard link to an earlier file of the archive (its path in the archive).
    HardLink(String),
}

/// What is written at one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    File(Vec<u8>),
    Dir,
    /// A symbolic link's target text (relative, inside the tree).
    Link(String),
}

/// The checked tree: `/`-separated relative paths to what is written there.
#[derive(Debug)]
pub struct Tree {
    nodes: BTreeMap<String, Node>,
    /// [`clash_key`] of each path (and of each directory above one) → that path.
    keys: HashMap<String, String>,
    limits: Limits,
    entries: usize,
    bytes: u64,
    /// The bytes of every path (and directory above one) recorded.
    names: usize,
}

impl Tree {
    pub(super) fn new(limits: Limits) -> Tree {
        Tree {
            nodes: BTreeMap::new(),
            keys: HashMap::new(),
            limits,
            entries: 0,
            bytes: 0,
            names: 0,
        }
    }

    /// The paths, sorted, with what is written at each.
    pub fn nodes(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.nodes.iter().map(|(p, n)| (p.as_str(), n))
    }

    /// The bytes of every file together.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// One more entry: `Err` naming it when the archive is refused.
    pub(super) fn add(&mut self, raw: &str, entry: Entry, packed: usize) -> Result<(), String> {
        self.entries += 1;
        if self.entries > self.limits.entries {
            return Err(format!(
                "it holds more than {} entries",
                self.limits.entries
            ));
        }
        let at = |why: String| format!("`{}`: {why}", raw.escape_debug());
        let Some(path) = names::clean(raw).map_err(at)? else {
            // The archive's root itself (`./`).
            return match entry {
                Entry::Dir => Ok(()),
                _ => Err(at("the archive's root must be a directory".into())),
            };
        };
        let node = match entry {
            Entry::File(bytes) => Node::File(bytes),
            Entry::Dir => Node::Dir,
            Entry::Symlink(target) => {
                names::check_link_target(&target).map_err(at)?;
                Node::Link(target)
            }
            Entry::HardLink(target) => Node::File(self.hard_link(&target).map_err(at)?),
        };
        if let Node::File(bytes) = &node {
            self.bytes = self.bytes.saturating_add(bytes.len() as u64);
            self.limits.check_size(self.bytes, packed)?;
        }
        self.place(&path, node).map_err(at)
    }

    /// The bytes of the earlier file `target` names.
    fn hard_link(&self, target: &str) -> Result<Vec<u8>, String> {
        let path = names::clean(target)
            .map_err(|e| format!("its hard link target: {e}"))?
            .unwrap_or_default();
        match self.nodes.get(&path) {
            Some(Node::File(bytes)) => Ok(bytes.clone()),
            _ => Err(format!(
                "a hard link to `{}`, which is not a file earlier in the archive",
                target.escape_debug()
            )),
        }
    }

    /// Put `node` at `path`, with every directory above it.
    fn place(&mut self, path: &str, node: Node) -> Result<(), String> {
        let mut end = 0;
        while let Some(slash) = path[end..].find('/') {
            end += slash;
            self.claim(&path[..end], &Node::Dir)?;
            end += 1;
        }
        self.claim(path, &node)?;
        self.nodes.insert(path.to_string(), node);
        Ok(())
    }

    /// Record that `path` is `node`'s kind, refusing a clash with a path already there.
    fn claim(&mut self, path: &str, node: &Node) -> Result<(), String> {
        let key = clash_key(path);
        match self.keys.get(&key) {
            Some(other) if other != path => {
                return Err(format!(
                    "`{}` and `{}` differ only by case or Unicode normalisation, so they are one \
                     file on macOS and Windows",
                    other.escape_debug(),
                    path.escape_debug()
                ))
            }
            Some(_) => match (self.nodes.get(path), node) {
                // A directory above several entries, or listed again.
                (Some(Node::Dir) | None, Node::Dir) => return Ok(()),
                (Some(Node::Link(_)), Node::Dir) => {
                    return Err(format!(
                        "`{}` is a link, so nothing can be under it",
                        path.escape_debug()
                    ))
                }
                (Some(Node::Dir) | None, _) => {
                    return Err(format!(
                        "`{}` is a directory and also a file or link",
                        path.escape_debug()
                    ))
                }
                (Some(_), Node::Dir) => {
                    return Err(format!(
                        "`{}` is a file, so nothing can be under it",
                        path.escape_debug()
                    ))
                }
                (Some(_), _) => return Err("the archive holds this path twice".to_string()),
            },
            None => {}
        }
        // Every directory above a path is recorded too: deep paths with new prefixes would cost
        // memory quadratic in their length, which no file size counts.
        self.names += path.len();
        if self.names > MAX_NAME_BYTES {
            return Err(format!(
                "its paths add up to more than {} MiB",
                MAX_NAME_BYTES >> 20
            ));
        }
        self.keys.insert(key, path.to_string());
        if *node == Node::Dir {
            self.nodes.entry(path.to_string()).or_insert(Node::Dir);
        }
        Ok(())
    }

    /// Check what is known only once every entry is in: where each link leads.
    pub(super) fn finish(&mut self) -> Result<(), String> {
        // By clash key: on macOS and Windows `d/UP` is the link `d/up`.
        let links: HashMap<String, &str> = (self.nodes.iter())
            .filter_map(|(p, n)| match n {
                Node::Link(t) => Some((clash_key(p), t.as_str())),
                _ => None,
            })
            .collect();
        let mut steps = 0;
        for (path, node) in &self.nodes {
            let Node::Link(target) = node else {
                continue;
            };
            let parent: Vec<&str> = match path.rsplit_once('/') {
                Some((dir, _)) => dir.split('/').collect(),
                None => vec![],
            };
            resolve(parent, target, &links, 0, &mut steps).map_err(|why| {
                format!(
                    "`{}`: a link to `{}`, which {why}",
                    path.escape_debug(),
                    target.escape_debug()
                )
            })?;
        }
        Ok(())
    }
}

/// How many links one lookup may pass through (Linux's limit).
const MAX_LINK_DEPTH: usize = 40;
/// How many path segments following every link of an archive may take: links to links with long
/// targets multiply (`b` → `a/a/…`, `c` → `b/b/…`).
const MAX_LINK_STEPS: usize = 1_000_000;

/// Follow `target` from the directory `at`, through the archive's `links` (by [`clash_key`]), as
/// the OS would: the path it leads to, or why it leaves the tree. A target is checked through
/// other links, not by its text alone: `a/l/../..` with `a/l` a link to `..` leaves a tree its
/// text stays in.
fn resolve<'a>(
    mut at: Vec<&'a str>,
    target: &'a str,
    links: &HashMap<String, &'a str>,
    depth: usize,
    steps: &mut usize,
) -> Result<Vec<&'a str>, String> {
    if depth > MAX_LINK_DEPTH {
        return Err(format!(
            "passes through more than {MAX_LINK_DEPTH} links (a loop?)"
        ));
    }
    for segment in target.split('/') {
        *steps += 1;
        if *steps > MAX_LINK_STEPS {
            return Err("takes too many steps to follow, with the archive's other links".into());
        }
        match segment {
            "" | "." => {}
            ".." => {
                if at.pop().is_none() {
                    return Err("leads outside the archive".to_string());
                }
            }
            name => {
                at.push(name);
                if let Some(next) = links.get(&clash_key(&at.join("/"))) {
                    at.pop();
                    at = resolve(at, next, links, depth + 1, steps)?;
                }
            }
        }
    }
    Ok(at)
}
