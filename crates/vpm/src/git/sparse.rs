//! Sparse checkouts (#3786): a git dependency with a `path` fetches and checks out that directory
//! of the commit alone, not the repository's whole tree. A dependency on one package of a large
//! repository then costs that package's files: nothing else is downloaded, written, checked or
//! hashed.
//!
//! What is checked out is what the package reaches in its commit, found from the commit's own
//! listing and manifests, so it is the same set on every machine:
//!
//! - the dependency's `path`;
//! - what npm's lookups read above it ([`around`]): each directory above's `package.json`,
//!   `pnpm-workspace.yaml` and `node_modules`, and the package directories of each npm workspace
//!   rooted there, which a package imports by name (`crate::npm`);
//! - what the symbolic links in all of those lead to (a link's target, and each link on the way
//!   to it).
//!
//! A package's `path` dependencies are packages of their own, each with its own checkout. The
//! files keep their places in the repository (`<checkout>/<path>/…`), so relative paths between
//! them hold, and the lock's checksum covers exactly these files ([`super::tree_checksum`] of
//! the checkout).
//!
//! The fetch is shallow (the one commit) and partial (`--filter=blob:none`: the commit and its
//! directory listings, no file contents); the files wanted are then asked for by their ids. A
//! server without partial fetches sends every file with the commit, and a git too old to ask for
//! files by id fetches the commit whole; the checkout is the same either way.
//!
//! `<checkout>.only`, beside the checkout and outside the hashed tree, lists what it holds: the
//! repository's URL, then one path per line. [`outside`] reads it to say why a file of the
//! repository is not there.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use sha2::{Digest, Sha256};

use super::{check_links, command, git, git_with, links_file, output, Config, Scratch};

/// How many rounds of following links a checkout takes before giving up: each round adds what
/// the links of the last one lead to.
const MAX_ROUNDS: usize = 64;

/// The name part that tells one path's checkout of a commit from another's.
pub(super) fn path_hash(path: &str) -> String {
    let hash = Sha256::digest(path.as_bytes());
    hash[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// `<checkout>.only`.
fn only_file(checkout: &Path) -> PathBuf {
    let mut name = checkout.file_name().unwrap_or_default().to_os_string();
    name.push(".only");
    checkout.with_file_name(name)
}

/// Whether `path` is `root` or below it (`/`-separated; an empty `root` is the repository).
fn within(path: &str, root: &str) -> bool {
    root.is_empty()
        || path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A commit's files, as `git ls-tree -r` lists them: each path with its object id, and whether
/// it is a symbolic link.
#[derive(Default)]
pub(super) struct Listing {
    files: BTreeMap<String, (String, bool)>,
}

impl Listing {
    /// From `git ls-tree -r -z`: `<mode> <type> <id>\t<path>` entries, NUL-separated.
    pub(super) fn parse(listing: &str) -> Listing {
        let files = listing
            .split('\0')
            .filter_map(|entry| {
                let (meta, path) = entry.split_once('\t')?;
                let mut meta = meta.split(' ');
                let (mode, kind, id) = (meta.next()?, meta.next()?, meta.next()?);
                // A submodule (`commit`) has no files here.
                (kind == "blob").then(|| (path.to_string(), (id.to_string(), mode == "120000")))
            })
            .collect();
        Listing { files }
    }

    fn is_link(&self, path: &str) -> bool {
        self.files.get(path).is_some_and(|(_, link)| *link)
    }

    /// Whether the commit has a file at `path` or below it.
    fn has(&self, path: &str) -> bool {
        self.under(path).next().is_some()
    }

    /// The files at `root` or below it, in path order.
    fn under<'a>(&'a self, root: &'a str) -> impl Iterator<Item = (&'a str, &'a str, bool)> {
        self.files
            .range(root.to_string()..)
            .take_while(move |(path, _)| root.is_empty() || path.starts_with(root))
            .filter(move |(path, _)| within(path, root))
            .map(|(path, (id, link))| (path.as_str(), id.as_str(), *link))
    }
}

/// Where a symbolic link leads in its commit.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Leads {
    /// This path of the repository (empty for its root), which may not exist.
    To(String),
    /// Through this other link, whose text isn't known yet.
    Through(String),
    /// Out of the repository.
    Out,
    /// Through more than 40 links.
    Loop,
}

/// Where the link at `link` leads, following the commit's other links by their `texts` (as
/// [`super::check_links`] follows a checkout's: at most 40, a part that doesn't exist taken as
/// written).
pub(super) fn leads(listing: &Listing, texts: &BTreeMap<String, String>, link: &str) -> Leads {
    let mut at: Vec<String> = link.split('/').map(str::to_string).collect();
    at.pop();
    let Some(text) = texts.get(link) else {
        return Leads::Through(link.to_string());
    };
    if text.starts_with('/') {
        return Leads::Out;
    }
    let mut rest: VecDeque<String> = text.split('/').map(str::to_string).collect();
    let mut hops = 0;
    while let Some(part) = rest.pop_front() {
        match part.as_str() {
            "" | "." => {}
            ".." => {
                if at.pop().is_none() {
                    return Leads::Out;
                }
            }
            // A drive or a separator git doesn't use: no path of the repository.
            name if name.contains(['\\', ':']) => return Leads::Out,
            name => {
                at.push(name.to_string());
                let path = at.join("/");
                if !listing.is_link(&path) {
                    continue;
                }
                let Some(next) = texts.get(&path) else {
                    return Leads::Through(path);
                };
                hops += 1;
                if hops > 40 {
                    return Leads::Loop;
                }
                if next.starts_with('/') {
                    return Leads::Out;
                }
                at.pop();
                for part in next.rsplit('/') {
                    rest.push_front(part.to_string());
                }
            }
        }
    }
    Leads::To(at.join("/"))
}

/// The scratch repository a checkout is made from.
struct Fetched<'a> {
    repo: &'a Path,
    url: &'a str,
    commit: &'a str,
    /// Ids of objects of the commit that the fetch left out.
    missing: HashSet<String>,
}

impl<'a> Fetched<'a> {
    /// A new bare repository at `repo` holding `commit` of `url` alone, without its files where
    /// the server can leave them out (`partial`), else with them.
    fn new(
        repo: &'a Path,
        url: &'a str,
        commit: &'a str,
        partial: bool,
    ) -> Result<Fetched<'a>, String> {
        let _ = std::fs::remove_dir_all(repo);
        let repo_arg = repo.to_string_lossy().into_owned();
        // No template: hooks from the user's `init.templateDir` would run at the checkout.
        git(None, &["init", "-q", "--bare", "--template=", &repo_arg])?;
        // A named remote: git notes on it that objects left out can be asked for.
        git(Some(repo), &["remote", "add", "origin", url])?;
        let mut args = vec!["fetch", "-q", "--depth", "1", "--no-tags"];
        if partial {
            args.push("--filter=blob:none");
        }
        args.extend(["origin", commit]);
        git(Some(repo), &args)?;
        let mut fetched = Fetched {
            repo,
            url,
            commit,
            missing: HashSet::new(),
        };
        if partial {
            // Each object the commit names and the repository lacks, as `?<id>`. A git that
            // can't list them left nothing out either.
            let listed = git(
                Some(repo),
                &["rev-list", "--objects", "--missing=print", commit],
            )
            .unwrap_or_default();
            fetched.missing = listed
                .lines()
                .filter_map(|l| l.strip_prefix('?'))
                .map(|id| id.trim().to_string())
                .collect();
        }
        Ok(fetched)
    }

    /// Fetch the objects `ids` that the repository lacks, asked for by id.
    fn fetch(&mut self, ids: &[&str]) -> Result<(), String> {
        let wanted: Vec<&str> = ids
            .iter()
            .copied()
            .filter(|id| self.missing.contains(*id))
            .collect();
        if wanted.is_empty() {
            return Ok(());
        }
        let args = [
            "-c",
            "fetch.negotiationAlgorithm=noop",
            "fetch",
            "-q",
            "--no-tags",
            "--no-write-fetch-head",
            "--recurse-submodules=no",
            "--filter=blob:none",
            "--stdin",
            "origin",
        ];
        let mut child = command(Config::User, Some(self.repo), &args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            // git reads the ids before it answers; a failed write shows as git's own error.
            let _ = stdin.write_all(wanted.join("\n").as_bytes());
        }
        output(child.wait_with_output())?;
        for id in wanted {
            self.missing.remove(id);
        }
        Ok(())
    }

    /// The text of the file stored as object `id` (a symbolic link's, a manifest's).
    fn text(&self, id: &str) -> Result<String, String> {
        git(Some(self.repo), &["cat-file", "blob", id])
    }
}

/// Fetch directory `path` of `commit` of the repository at `url`, and what its symbolic links
/// lead to, into a staging directory, and move that into place at `dir`: a checkout in the cache
/// is always complete.
pub(super) fn fetch(url: &str, commit: &str, path: &str, dir: &Path) -> Result<(), String> {
    fetch_keeping(url, commit, path, dir, None)
}

/// [`fetch`], with the scratch repository at `keep` and left there when given (for a test to
/// look at what was fetched).
pub(super) fn fetch_keeping(
    url: &str,
    commit: &str,
    path: &str,
    dir: &Path,
    keep: Option<&Path>,
) -> Result<(), String> {
    let parent = dir.parent().expect("ICE: a checkout has a parent");
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create `{}`: {e}", parent.display()))?;
    let scratch = Scratch(parent.join(format!(
        ".fetch-{commit}-{}-{}",
        path_hash(path),
        std::process::id()
    )));
    let _ = std::fs::remove_dir_all(&scratch.0);
    let repo = keep.map_or_else(|| scratch.0.join("repo"), Path::to_path_buf);
    let tree = scratch.0.join("tree");
    std::fs::create_dir_all(&tree)
        .map_err(|e| format!("cannot create `{}`: {e}", tree.display()))?;
    let cannot_fetch = |e: String| format!("cannot fetch commit {commit} of {url}: {e}");
    let mut fetched = Fetched::new(&repo, url, commit, true)
        .or_else(|_| Fetched::new(&repo, url, commit, false))
        .map_err(cannot_fetch)?;
    let skeleton = scratch.0.join("skeleton");
    let (listing, paths) = match files(&mut fetched, path, &skeleton) {
        Ok(found) => found,
        // A git that can't ask for files by id, or a server that won't send them: the commit
        // with all its files, as a whole checkout fetches it.
        Err(Failed::Fetch(_)) => {
            fetched = Fetched::new(&repo, url, commit, false).map_err(cannot_fetch)?;
            files(&mut fetched, path, &skeleton).map_err(|e| e.message(url, commit))?
        }
        Err(e) => return Err(e.message(url, commit)),
    };
    let held: Vec<&str> = paths.iter().map(String::as_str).collect();
    if held.iter().any(|p| listing.has(p)) {
        let work_tree = format!("--work-tree={}", tree.display());
        let no_hooks = format!("core.hooksPath={}", scratch.0.join("no-hooks").display());
        // Each path as it is written, not as a pattern.
        let specs: Vec<String> = held
            .iter()
            .filter(|p| listing.has(p))
            .map(|p| match p.is_empty() {
                true => ".".to_string(),
                false => format!(":(literal){p}"),
            })
            .collect();
        let mut args = vec![
            "-c", &no_hooks, &work_tree, "checkout", "-q", "-f", commit, "--",
        ];
        args.extend(specs.iter().map(String::as_str));
        git_with(Config::Own, Some(&repo), &args)
            .map_err(|e| format!("cannot check out `{path}` of commit {commit} of {url}: {e}"))?;
    }
    check_links(&tree).map_err(|e| format!("commit {commit} of {url} {e}"))?;
    let links: Vec<&str> = held
        .iter()
        .flat_map(|p| listing.under(p))
        .filter(|(_, _, link)| *link)
        .map(|(path, _, _)| path)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let list = links_file(dir);
    std::fs::write(&list, links.join("\n"))
        .map_err(|e| format!("cannot write `{}`: {e}", list.display()))?;
    let only = only_file(dir);
    std::fs::write(&only, format!("{url}\n{}\n", held.join("\n")))
        .map_err(|e| format!("cannot write `{}`: {e}", only.display()))?;
    match std::fs::rename(&tree, dir) {
        Ok(()) => Ok(()),
        // Another `velt` put the same checkout in place meanwhile.
        Err(_) if dir.is_dir() => Ok(()),
        Err(e) => Err(format!(
            "cannot move the checkout to `{}`: {e}",
            dir.display()
        )),
    }
}

/// Why the files of a checkout could not be found or fetched.
enum Failed {
    /// Asking for files by id failed.
    Fetch(String),
    /// A link of the commit can't be followed.
    Link(String),
    Other(String),
}

impl Failed {
    fn message(self, url: &str, commit: &str) -> String {
        match self {
            Failed::Fetch(e) => format!("cannot fetch commit {commit} of {url}: {e}"),
            Failed::Link(e) => format!("commit {commit} of {url} {e}"),
            Failed::Other(e) => e,
        }
    }
}

/// The commit's listing and the paths a checkout of `path` holds: `path`, what npm's lookups
/// read above it ([`around`]), and what the links in those lead to, and the links in those.
/// Every file below them is in the repository when this returns. `skeleton` is a directory to
/// work in.
fn files(
    fetched: &mut Fetched,
    path: &str,
    skeleton: &Path,
) -> Result<(Listing, BTreeSet<String>), Failed> {
    let (url, commit) = (fetched.url, fetched.commit);
    let listing = git(
        Some(fetched.repo),
        &["ls-tree", "-r", "-z", "--full-tree", commit],
    )
    .map_err(|e| Failed::Other(format!("cannot list commit {commit} of {url}: {e}")))?;
    let listing = Listing::parse(&listing);
    let mut paths = BTreeSet::from([path.to_string()]);
    hold(&mut paths, around(fetched, &listing, path, skeleton)?);
    let mut texts = BTreeMap::new();
    for _ in 0..MAX_ROUNDS {
        let wanted: Vec<(&str, &str, bool)> = paths.iter().flat_map(|p| listing.under(p)).collect();
        let ids: Vec<&str> = wanted.iter().map(|(_, id, _)| *id).collect();
        fetched.fetch(&ids).map_err(Failed::Fetch)?;
        let links: Vec<(&str, &str)> = wanted
            .iter()
            .filter(|(_, _, link)| *link)
            .map(|(path, id, _)| (*path, *id))
            .collect();
        for (link, id) in &links {
            if !texts.contains_key(*link) {
                let text = fetched.text(id).map_err(Failed::Other)?;
                texts.insert(link.to_string(), text);
            }
        }
        let mut more = BTreeSet::new();
        for (link, _) in links {
            let to = match leads(&listing, &texts, link) {
                Leads::To(to) | Leads::Through(to) => to,
                why => {
                    let what = match why {
                        Leads::Loop => "loops",
                        _ => "points out of the repository",
                    };
                    return Err(Failed::Link(format!(
                        "has a symbolic link `{link}` that {what} (`{}`)",
                        texts[link]
                    )));
                }
            };
            let held = paths.iter().any(|p| within(&to, p));
            if !held && listing.has(&to) {
                more.insert(to);
            }
        }
        if more.is_empty() {
            return Ok((listing, paths));
        }
        hold(&mut paths, more);
    }
    Err(Failed::Link(format!(
        "has symbolic links below `{path}` that lead on through more than {MAX_ROUNDS} others"
    )))
}

/// Add `more` to the paths a checkout holds: a path at or below one that is held is not named
/// again, and what a new path holds, an earlier one below it need not name.
fn hold(paths: &mut BTreeSet<String>, more: impl IntoIterator<Item = String>) {
    for path in more {
        if paths.iter().any(|p| within(&path, p)) {
            continue;
        }
        paths.retain(|p| !within(p, &path));
        paths.insert(path);
    }
}

/// The directories above `path` in the repository, nearest first, ending with its root (``).
fn above(path: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut at = path;
    while let Some((parent, _)) = at.rsplit_once('/') {
        out.push(parent);
        at = parent;
    }
    out.push("");
    out
}

/// What npm's lookups for a package at `path` read above it in the commit (`crate::npm`: a
/// package imports another of its workspace by name, as sigx's Velt package imports its sibling
/// TypeScript packages): each directory above's `package.json`, `pnpm-workspace.yaml` and
/// `node_modules`, and the package directories of each workspace rooted there. The workspaces'
/// patterns are read as a checkout's are, over `skeleton`: the manifests above `path` and a
/// stand-in for every other `package.json` of the commit.
fn around(
    fetched: &mut Fetched,
    listing: &Listing,
    path: &str,
    skeleton: &Path,
) -> Result<Vec<String>, Failed> {
    use crate::npm::workspace;
    let join = |dir: &str, name: &str| match dir.is_empty() {
        true => name.to_string(),
        false => format!("{dir}/{name}"),
    };
    let mut out = vec![];
    let mut manifests = vec![];
    for dir in above(path) {
        let modules = join(dir, "node_modules");
        if listing.has(&modules) {
            out.push(modules);
        }
        for name in ["package.json", workspace::PNPM_WORKSPACE] {
            let file = join(dir, name);
            if let Some((id, false)) = listing.files.get(&file) {
                manifests.push((dir, file, id.as_str()));
            }
        }
    }
    if manifests.is_empty() {
        return Ok(out);
    }
    let ids: Vec<&str> = manifests.iter().map(|(_, _, id)| *id).collect();
    fetched.fetch(&ids).map_err(Failed::Fetch)?;
    let write = |rel: &str, text: &str| {
        let file = skeleton.join(rel);
        let made = match file.parent() {
            Some(parent) => std::fs::create_dir_all(parent),
            None => Ok(()),
        };
        made.and_then(|()| std::fs::write(&file, text))
            .map_err(|e| Failed::Other(format!("cannot write `{}`: {e}", file.display())))
    };
    // Which directories hold a `package.json` is all a workspace's patterns ask of the others.
    for file in listing.files.keys() {
        if file == "package.json" || file.ends_with("/package.json") {
            write(file, "{}")?;
        }
    }
    for (_, file, id) in &manifests {
        let text = fetched.text(id).map_err(Failed::Other)?;
        write(file, &text)?;
    }
    for (dir, file, _) in &manifests {
        out.push(file.clone());
        let root = skeleton.join(dir);
        // Once per directory, whichever of its two manifests names the workspace.
        let first = manifests.iter().find(|(d, _, _)| d == dir);
        if first.is_some_and(|(_, f, _)| f != file) {
            continue;
        }
        let Some(patterns) = workspace::patterns(&root) else {
            continue;
        };
        for member in workspace::package_dirs(&root, &patterns).dirs {
            let rel = crate::relpath::relative(&member, skeleton);
            out.push(if rel == "." { String::new() } else { rel });
        }
    }
    Ok(out)
}

/// For a file that isn't there: when `file` is inside a sparse checkout and outside what it
/// holds, a note saying so, naming the file's path in the repository.
pub fn outside(file: &Path) -> Option<String> {
    for root in file.ancestors().skip(1) {
        root.file_name()?;
        let Ok(text) = std::fs::read_to_string(only_file(root)) else {
            continue;
        };
        let rel = file.strip_prefix(root).ok()?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        let mut lines = text.lines();
        let url = lines.next()?;
        let held: Vec<&str> = lines.collect();
        if held.iter().any(|p| within(&rel, p)) {
            return None;
        }
        let held: Vec<String> = held.iter().map(|p| format!("`{p}`")).collect();
        return Some(format!(
            "`{rel}` is outside what this git dependency checks out of {url}: a dependency with a `path` fetches that directory alone ({}); a package reaches other files of its repository through a `path` dependency on the package that holds them",
            held.join(", ")
        ));
    }
    None
}
