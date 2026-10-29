//! Git dependencies (#1860): a package in a git repository, at a branch, a tag or a commit.
//!
//! ```ts ignore
//! dependencies: {
//!   sigx: { git: "https://github.com/signalxjs/core", branch: "main", path: "packages/velt/velt" },
//! }
//! ```
//!
//! A branch, a tag or the default branch is resolved to a commit (`git ls-remote`) once, and
//! `velt.lock.json` records the commit with a checksum of the checkout: builds use the locked
//! commit without the network, `velt update` moves it. A checkout is the commit's whole tree
//! (without `.git`), fetched with the `git` command (so the machine's credentials and SSH keys
//! apply), stored once per commit under `<cache>/git/<repository>/<commit>/` and never changed.
//! The whole tree is hashed, not only the package's files: a package's npm sources may live
//! beside it in the repository (sigx's Velt half imports its sibling TypeScript packages).
//! Nothing in the checkout runs.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::locations::Locations;
use crate::manifest::{Dependency, DetailedDependency};

/// Which commit of the repository a dependency follows.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitRef {
    /// The repository's default branch (`HEAD`).
    Default,
    Branch(String),
    Tag(String),
    /// A full commit hash.
    Rev(String),
}

/// A git dependency's source: the repository, which commit it follows, and the package's
/// directory in it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitSpec {
    pub url: String,
    pub reference: GitRef,
    /// The package's directory relative to the repository root (`/`-separated), `None` for the
    /// root.
    pub subdir: Option<String>,
}

/// The `source` prefix of a git package in `velt.lock.json`.
pub const GIT_SOURCE: &str = "git";

impl GitSpec {
    /// The lockfile's `source` for this spec at `commit`:
    /// `git+<url>?branch=main&path=packages/x#<commit>`.
    pub fn source(&self, commit: &str) -> String {
        let mut query = vec![];
        match &self.reference {
            GitRef::Default => {}
            GitRef::Branch(b) => query.push(format!("branch={b}")),
            GitRef::Tag(t) => query.push(format!("tag={t}")),
            GitRef::Rev(r) => query.push(format!("rev={r}")),
        }
        if let Some(dir) = &self.subdir {
            query.push(format!("path={dir}"));
        }
        let query = match query.is_empty() {
            true => String::new(),
            false => format!("?{}", query.join("&")),
        };
        format!("{GIT_SOURCE}+{}{query}#{commit}", self.url)
    }

    /// The spec and commit of a lockfile `source` written by [`GitSpec::source`].
    pub fn parse_source(source: &str) -> Option<(GitSpec, String)> {
        let rest = source.strip_prefix(GIT_SOURCE)?.strip_prefix('+')?;
        let (rest, commit) = rest.rsplit_once('#')?;
        let (url, query) = match rest.split_once('?') {
            Some((url, query)) => (url, query),
            None => (rest, ""),
        };
        let mut spec = GitSpec {
            url: url.to_string(),
            reference: GitRef::Default,
            subdir: None,
        };
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=')?;
            let value = value.to_string();
            match key {
                "branch" => spec.reference = GitRef::Branch(value),
                "tag" => spec.reference = GitRef::Tag(value),
                "rev" => spec.reference = GitRef::Rev(value),
                "path" => spec.subdir = Some(value),
                _ => return None,
            }
        }
        is_commit(commit).then(|| (spec, commit.to_string()))
    }

    /// How the spec reads in messages: `https://github.com/x/y (branch main)`.
    pub fn describe(&self) -> String {
        let at = match &self.reference {
            GitRef::Default => String::new(),
            GitRef::Branch(b) => format!(" (branch {b})"),
            GitRef::Tag(t) => format!(" (tag {t})"),
            GitRef::Rev(r) => format!(" (commit {})", &r[..r.len().min(12)]),
        };
        let path = self
            .subdir
            .as_ref()
            .map(|d| format!(", path {d}"))
            .unwrap_or_default();
        format!("{}{at}{path}", self.url)
    }
}

/// Whether `s` is a full commit hash (SHA-1 or SHA-256, lowercase hex).
pub fn is_commit(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `url` is a repository URL `velt` hands to git: `https://`, `http://`, `ssh://`,
/// `git://`, `file://`, or scp-like `git@host:path`. Nothing that git could take for an option
/// or for another transport (`ext::`).
pub fn check_url(url: &str) -> Result<(), String> {
    let schemes = ["https://", "http://", "ssh://", "git://", "file://"];
    let scp = url
        .split_once(':')
        .is_some_and(|(host, path)| host.contains('@') && !host.contains('/') && !path.is_empty());
    let ok = (schemes.iter().any(|s| url.starts_with(s)) || scp)
        && !url.starts_with('-')
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '#' || c == '?');
    match ok {
        true => Ok(()),
        false => Err(format!(
            "`{url}` is not a git repository URL (https://…, ssh://…, git@host:path or file://…, without `?` or `#`)"
        )),
    }
}

/// The commit `spec`'s reference names now, asked of the repository.
pub fn resolve_ref(spec: &GitSpec) -> Result<String, String> {
    let pattern = match &spec.reference {
        GitRef::Rev(rev) => return Ok(rev.clone()),
        GitRef::Default => "HEAD".to_string(),
        GitRef::Branch(b) => format!("refs/heads/{b}"),
        GitRef::Tag(t) => format!("refs/tags/{t}"),
    };
    let out = git(None, &["ls-remote", "--", &spec.url, &pattern])
        .map_err(|e| format!("cannot reach {}: {e}", spec.url))?;
    // An annotated tag is listed twice: the tag object, and `^{}` with the commit it points to.
    let mut found = None;
    for line in out.lines() {
        let Some((hash, name)) = line.split_once('\t') else {
            continue;
        };
        if name == format!("{pattern}^{{}}") {
            found = Some(hash);
            break;
        }
        if name == pattern && found.is_none() {
            found = Some(hash);
        }
    }
    found
        .filter(|h| is_commit(h))
        .map(str::to_string)
        .ok_or_else(|| format!("{} has no {}", spec.url, describe_ref(&spec.reference)))
}

fn describe_ref(reference: &GitRef) -> String {
    match reference {
        GitRef::Default => "default branch".to_string(),
        GitRef::Branch(b) => format!("branch `{b}`"),
        GitRef::Tag(t) => format!("tag `{t}`"),
        GitRef::Rev(r) => format!("commit `{r}`"),
    }
}

/// The checkout of `commit` of the repository at `url`: from the cache, else fetched into it.
/// When `checksum` is given (from the lockfile), the checkout must hash to it.
pub fn checkout(
    loc: &Locations,
    url: &str,
    commit: &str,
    checksum: Option<&str>,
) -> Result<PathBuf, String> {
    let dir = repository_dir(loc, url).join(commit);
    if !dir.is_dir() {
        fetch(url, commit, &dir)?;
    }
    if let Some(expected) = checksum {
        let actual = tree_checksum(&dir)?;
        if actual != expected {
            return Err(format!(
                "checksum mismatch for commit {commit} of {url}: velt.lock.json has {expected}, the checkout hashes to {actual} (delete `{}` to fetch it again)",
                dir.display()
            ));
        }
    }
    Ok(dir)
}

/// `<cache>/git/<repository>`: named by a hash of the URL, so two URLs never share checkouts.
fn repository_dir(loc: &Locations, url: &str) -> PathBuf {
    let hash = Sha256::digest(url.as_bytes());
    let name: String = hash[..10].iter().map(|b| format!("{b:02x}")).collect();
    loc.cache.join("git").join(name)
}

/// Fetch `commit` alone into a scratch repository, check its tree out into a staging directory,
/// and move that into place: a checkout in the cache is always complete.
fn fetch(url: &str, commit: &str, dir: &Path) -> Result<(), String> {
    let parent = dir.parent().expect("ICE: a checkout has a parent");
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create `{}`: {e}", parent.display()))?;
    let scratch = Scratch(parent.join(format!(".fetch-{commit}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&scratch.0);
    let repo = scratch.0.join("repo");
    let tree = scratch.0.join("tree");
    std::fs::create_dir_all(&tree)
        .map_err(|e| format!("cannot create `{}`: {e}", tree.display()))?;
    let repo_arg = repo.to_string_lossy().into_owned();
    git(None, &["init", "-q", "--bare", &repo_arg])?;
    git(
        Some(&repo),
        &["fetch", "-q", "--depth", "1", "--", url, commit],
    )
    .map_err(|e| format!("cannot fetch commit {commit} of {url}: {e}"))?;
    let work_tree = format!("--work-tree={}", tree.display());
    git(
        Some(&repo),
        &[&work_tree, "checkout", "-q", "-f", commit, "--", "."],
    )
    .map_err(|e| format!("cannot check out commit {commit} of {url}: {e}"))?;
    match std::fs::rename(&tree, dir) {
        Ok(()) => Ok(()),
        // Another `velt` put the same commit in place meanwhile.
        Err(_) if dir.is_dir() => Ok(()),
        Err(e) => Err(format!(
            "cannot move the checkout to `{}`: {e}",
            dir.display()
        )),
    }
}

/// A directory removed when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run git (in `dir`, when given) without prompting for credentials; its stdout, or its stderr
/// as the error.
fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    let out = cmd.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            "git dependencies need the `git` command (https://git-scm.com)".to_string()
        }
        _ => format!("cannot run git: {e}"),
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err
            .trim()
            .lines()
            .last()
            .unwrap_or("git failed")
            .to_string())
    }
}

/// `sha256:<hex>` over every file of the checkout, as [`crate::contents::checksum`] hashes a
/// package's: for each file in path order, its `/`-separated relative path, a NUL, its length
/// and its bytes. Symbolic links are hashed as their target's text.
pub fn tree_checksum(root: &Path) -> Result<String, String> {
    let mut files = vec![];
    collect(root, "", &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for rel in files {
        let path = root.join(&rel);
        let bytes = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                std::fs::read_link(&path).map(|t| t.to_string_lossy().into_owned().into_bytes())
            }
            _ => std::fs::read(&path),
        }
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("sha256:{hex}"))
}

fn collect(dir: &Path, rel: &str, out: &mut Vec<String>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = match rel.is_empty() {
            true => name.clone(),
            false => format!("{rel}/{name}"),
        };
        let kind = entry
            .file_type()
            .map_err(|e| format!("cannot read `{}`: {e}", entry.path().display()))?;
        if kind.is_dir() {
            collect(&entry.path(), &child, out)?;
        } else {
            out.push(child);
        }
    }
    Ok(())
}

impl Dependency {
    /// The git source, if this is a git dependency.
    pub fn git(&self) -> Option<GitSpec> {
        let Dependency::Detailed(d) = self else {
            return None;
        };
        let reference = match (&d.branch, &d.tag, &d.rev) {
            (Some(b), _, _) => GitRef::Branch(b.clone()),
            (_, Some(t), _) => GitRef::Tag(t.clone()),
            (_, _, Some(r)) => GitRef::Rev(r.clone()),
            _ => GitRef::Default,
        };
        Some(GitSpec {
            url: d.git.clone()?,
            reference,
            subdir: d.path.clone(),
        })
    }
}

/// The git fields of dependency `name`: `branch`, `tag` and `rev` only with `git`, at most one of
/// them, a full commit hash in `rev`, and with `git` a `path` inside the repository.
pub(crate) fn check_fields(name: &str, d: &DetailedDependency) -> Result<(), String> {
    let refs = [("branch", &d.branch), ("tag", &d.tag), ("rev", &d.rev)];
    let given: Vec<&str> = refs
        .iter()
        .filter(|(_, v)| v.is_some())
        .map(|(k, _)| *k)
        .collect();
    let Some(url) = &d.git else {
        return match given.first() {
            Some(key) => Err(format!(
                "dependency `{name}`: `{key}` needs a `git` repository"
            )),
            None => Ok(()),
        };
    };
    check_url(url).map_err(|e| format!("dependency `{name}`: {e}"))?;
    if given.len() > 1 {
        return Err(format!(
            "dependency `{name}`: `{}` and `{}` both name a commit; keep one",
            given[0], given[1]
        ));
    }
    if let Some(rev) = &d.rev {
        if !is_commit(rev) {
            return Err(format!(
                "dependency `{name}`: `rev` must be a full commit hash (40 hex digits), not `{rev}`"
            ));
        }
    }
    for (key, value) in [("branch", &d.branch), ("tag", &d.tag)] {
        if let Some(v) = value {
            if v.is_empty() || v.starts_with('-') || v.contains(char::is_whitespace) {
                return Err(format!(
                    "dependency `{name}`: `{key}` `{v}` is not a git ref name"
                ));
            }
        }
    }
    if let Some(path) = &d.path {
        let inside = !path.is_empty()
            && !path.starts_with('/')
            && !path.contains('\\')
            && path
                .split('/')
                .all(|s| !s.is_empty() && s != "." && s != "..");
        if !inside {
            return Err(format!(
                "dependency `{name}`: with `git`, `path` is a `/`-separated directory inside the repository, not `{path}`"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
