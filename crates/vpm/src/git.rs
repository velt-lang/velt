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

use std::ffi::OsString;
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
            GitRef::Branch(b) => query.push(format!("branch={}", encode(b))),
            GitRef::Tag(t) => query.push(format!("tag={}", encode(t))),
            GitRef::Rev(r) => query.push(format!("rev={r}")),
        }
        if let Some(dir) = &self.subdir {
            query.push(format!("path={}", encode(dir)));
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
            let value = decode(value)?;
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

/// `value` with the characters a lockfile source uses as delimiters (`&`, `=`, `#`, `?`) and `%`
/// percent-encoded: git allows them in branch and directory names.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '%' | '&' | '=' | '#' | '?' => out.push_str(&format!("%{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// [`encode`] undone; `None` for a malformed escape.
fn decode(value: &str) -> Option<String> {
    let mut out = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(b) = bytes.next() {
        if b != b'%' {
            out.push(b);
            continue;
        }
        let hex = [bytes.next()?, bytes.next()?];
        out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
    }
    String::from_utf8(out).ok()
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
    if !ok {
        return Err(format!(
            "`{url}` is not a git repository URL (https://…, ssh://…, git@host:path or file://…, without `?` or `#`)"
        ));
    }
    // A token in the URL would be printed in messages and written to `velt.lock.json`, which is
    // committed. `ssh://git@host/…` names a user, not a secret.
    let userinfo = url.split_once("://").and_then(|(scheme, rest)| {
        let authority = rest.split('/').next().unwrap_or("");
        authority.rsplit_once('@').map(|(info, _)| (scheme, info))
    });
    if let Some((scheme, info)) = userinfo {
        if info.contains(':') || scheme.starts_with("http") {
            return Err(
                "a git URL can't hold credentials (they would be printed and written to velt.lock.json); let git supply them with a credential helper (`git config credential.helper`) or use an SSH URL".to_string(),
            );
        }
    }
    Ok(())
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
/// When `checksum` is given (from the lockfile), the checkout must hash to it. A fetched commit's
/// links are checked once, before it enters the cache ([`check_links`]); a checkout already in the
/// cache is trusted as it is (only this velt writes `<cache>/git`).
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

/// The list of a checkout's symbolic links (`/`-separated paths, one per line), beside it and
/// outside the hashed tree: `<cache>/git/<repository>/<commit>.links`.
fn links_file(checkout: &Path) -> PathBuf {
    let mut name = checkout.file_name().unwrap_or_default().to_os_string();
    name.push(".links");
    checkout.with_file_name(name)
}

/// The symbolic links of the commit checked out at `checkout` (empty for a checkout made before
/// they were listed).
pub fn links(checkout: &Path) -> Vec<String> {
    std::fs::read_to_string(links_file(checkout))
        .map(|text| {
            text.lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// On Windows, a warning per symbolic link inside the package at `subdir` of the checkout (all
/// of it for `None`): it is a file holding its target there, so importing through it fails with
/// errors that don't say why. `windows` is whether this is Windows (a parameter for tests).
pub fn link_warnings(
    checkout: &Path,
    url: &str,
    subdir: Option<&str>,
    windows: bool,
) -> Vec<String> {
    if !windows {
        return vec![];
    }
    let prefix = subdir.map(|s| format!("{s}/")).unwrap_or_default();
    links(checkout)
        .into_iter()
        .filter(|link| link.starts_with(&prefix))
        .map(|link| {
            // A file holding the target on Windows; a link elsewhere (for tests).
            let path = checkout.join(&link);
            let target = std::fs::read_link(&path)
                .map(|t| t.to_string_lossy().into_owned())
                .or_else(|_| std::fs::read_to_string(&path))
                .unwrap_or_default();
            format!(
                "`{link}` is a symbolic link in {url}; on Windows a git dependency's links are checked out as files holding their target (`{}`), so it can't be imported or read through",
                target.trim()
            )
        })
        .collect()
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
    // No template: hooks from the user's `init.templateDir` would run at the checkout.
    git(None, &["init", "-q", "--bare", "--template=", &repo_arg])?;
    git(
        Some(&repo),
        &["fetch", "-q", "--depth", "1", "--", url, commit],
    )
    .map_err(|e| format!("cannot fetch commit {commit} of {url}: {e}"))?;
    let work_tree = format!("--work-tree={}", tree.display());
    let no_hooks = format!("core.hooksPath={}", scratch.0.join("no-hooks").display());
    git_with(
        Config::Own,
        Some(&repo),
        &[
            "-c", &no_hooks, &work_tree, "checkout", "-q", "-f", commit, "--", ".",
        ],
    )
    .map_err(|e| format!("cannot check out commit {commit} of {url}: {e}"))?;
    check_links(&tree).map_err(|e| format!("commit {commit} of {url} {e}"))?;
    // The commit's links, as git lists them: on Windows they are checked out as files, and a
    // package that uses one is told why ([`link_warnings`]).
    let listing = git(Some(&repo), &["ls-tree", "-r", "-z", "--full-tree", commit])
        .map_err(|e| format!("cannot list commit {commit} of {url}: {e}"))?;
    let links: Vec<&str> = listing
        .split('\0')
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            meta.starts_with("120000 ").then_some(path)
        })
        .collect();
    let list = links_file(dir);
    std::fs::write(&list, links.join("\n"))
        .map_err(|e| format!("cannot write `{}`: {e}", list.display()))?;
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

/// Every symbolic link in the checkout at `tree` must point inside it: the checksum covers a
/// link's text, not what it points to, so a link out of the repository (`/etc/…`, `../../x`, a
/// file elsewhere on this machine) would build what the lock doesn't pin. Each link is resolved
/// inside the checkout, following the checkout's own links, so a chain can't leave it either; a
/// part that doesn't exist yet is taken as written.
fn check_links(tree: &Path) -> Result<(), String> {
    let mut files = vec![];
    collect(tree, "", &mut files)?;
    for rel in files {
        let path = tree.join(&rel);
        let is_link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
        if !is_link {
            continue;
        }
        let target = std::fs::read_link(&path)
            .map_err(|e| format!("has a symbolic link `{rel}` that can't be read: {e}"))?;
        let dir: Vec<OsString> = rel.split('/').map(OsString::from).collect();
        let dir = dir[..dir.len() - 1].to_vec();
        if let Err(why) = resolve_inside(tree, dir, &target, &mut 0) {
            let what = match why {
                Unresolved::Out => "points out of the repository",
                Unresolved::Loop => "loops",
            };
            return Err(format!(
                "has a symbolic link `{rel}` that {what} (`{}`)",
                target.display()
            ));
        }
    }
    Ok(())
}

/// Why a link doesn't resolve inside the checkout.
enum Unresolved {
    Out,
    /// More than 40 links followed.
    Loop,
}

/// Where `target`, a link's text in the checkout's directory `at` (its components), leads inside
/// the checkout at `tree`, following the checkout's links (at most 40 of them).
fn resolve_inside(
    tree: &Path,
    mut at: Vec<OsString>,
    target: &Path,
    hops: &mut u32,
) -> Result<Vec<OsString>, Unresolved> {
    use std::path::Component;
    for c in target.components() {
        match c {
            Component::Normal(name) => {
                at.push(name.to_os_string());
                let path = tree.join(at.iter().collect::<PathBuf>());
                let is_link =
                    std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
                if is_link {
                    *hops += 1;
                    if *hops > 40 {
                        return Err(Unresolved::Loop);
                    }
                    let next = std::fs::read_link(&path).map_err(|_| Unresolved::Out)?;
                    at.pop();
                    at = resolve_inside(tree, at, &next, hops)?;
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                at.pop().ok_or(Unresolved::Out)?;
            }
            Component::RootDir | Component::Prefix(_) => return Err(Unresolved::Out),
        }
    }
    Ok(at)
}

/// A directory removed when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Git's variables that point it at a repository (`git rev-parse --local-env-vars`): set inside a
/// git hook (a `pre-commit` running `velt test`), they would send velt's commands into the user's
/// own repository.
const LOCAL_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

/// Which configuration git reads: the user's (credential helpers, `insteadOf`, SSH commands: for
/// talking to a repository), or velt's alone (checking a commit out of velt's scratch repository,
/// so the files, and the lock's checksum, are the same on every machine, whatever `core.autocrlf`
/// or filters the user's configuration sets).
#[derive(Clone, Copy, PartialEq)]
enum Config {
    User,
    Own,
}

/// How the checkout writes symbolic links. On Windows, as plain files holding the text git stores
/// (as git does where links can't be made): Git for Windows would write their targets with `\`,
/// which the checksum would see, so a lock made there would not match one made elsewhere, and
/// making links needs a permission many Windows machines don't grant.
const SYMLINKS: &str = if cfg!(windows) {
    "core.symlinks=false"
} else {
    "core.symlinks=true"
};

/// Run git (in `dir`, when given) without prompting for credentials and without the
/// environment's repository ([`LOCAL_ENV`]); its stdout, or git's error as the error.
fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    git_with(Config::User, dir, args)
}

fn git_with(config: Config, dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    for var in LOCAL_ENV {
        cmd.env_remove(var);
    }
    if config == Config::Own {
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null)
            .args(["-c", "core.autocrlf=false", "-c", "core.eol=lf"])
            .args(["-c", SYMLINKS, "-c", "core.longpaths=true"]);
    }
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
        // git's own `fatal:` line says what went wrong; what ssh printed before it doesn't.
        let err = String::from_utf8_lossy(&out.stderr);
        let lines: Vec<&str> = err
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let line = lines
            .iter()
            .find(|l| l.starts_with("fatal:"))
            .or(lines.last())
            .copied()
            .unwrap_or("git failed");
        Err(line.to_string())
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
                "dependency `{name}`: `rev` must be a full commit hash in lowercase hex (40 digits, or 64 for SHA-256), not `{rev}`"
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
        // Plain directory names on every OS: no `C:` drive, no root, no `..`.
        let inside = path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != ".." && !s.contains(['\\', ':']))
            && Path::new(path)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
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
