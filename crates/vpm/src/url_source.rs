//! Dependencies from a release archive at a URL, pinned by its SHA-256 (#2367 section 1):
//! `{ url: "https://…/vue-3.6.0.tar.gz", sha256: "…", strip: 1, path: "packages/vue" }`.
//!
//! - **Download**: `https` on every redirect hop (or plain `http` to this machine, for tests and
//!   local mirrors), with the size capped while reading ([`MAX_ARCHIVE`]); the hash is checked on
//!   the final bytes before anything reads them.
//! - **Extraction**: [`crate::extract`], into `<cache>/url/<sha256>/`, the archive's raw tree. One
//!   archive can give several packages: `strip` and `path` choose a directory inside it, so
//!   each result is a directory of the raw tree, never a copy. Its links are checked again from
//!   that directory, since a link that stays in the archive can leave the chosen part.
//! - **Lock**: `url+<url>?strip=1&path=packages/vue#sha256:<hex>`, with the chosen tree's
//!   [`crate::git::tree_checksum`] in `checksum`, checked on every use as for git.
//!
//! Release assets only: a forge's generated archive of a tag can change bytes when the forge
//! changes compression, and the hash is strict. Use `git` with a commit for source.

use std::path::{Path, PathBuf};

use crate::locations::Locations;
use crate::manifest::{Dependency, DetailedDependency};

/// The `source` prefix of a url package in `velt.lock.json`.
pub const URL_SOURCE: &str = "url";

/// The largest archive downloaded.
pub const MAX_ARCHIVE: usize = 512 << 20;

/// Where a url package comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UrlSpec {
    pub url: String,
    /// Lowercase hex.
    pub sha256: String,
    /// Leading directories removed.
    pub strip: u32,
    /// The package's directory in the (stripped) archive, `/`-separated.
    pub subdir: Option<String>,
}

impl UrlSpec {
    /// The lockfile's `source`: `url+<url>?strip=1&path=x#sha256:<hex>`.
    pub fn source(&self) -> String {
        let mut query = vec![];
        if self.strip > 0 {
            query.push(format!("strip={}", self.strip));
        }
        if let Some(dir) = &self.subdir {
            query.push(format!("path={}", crate::git::encode(dir)));
        }
        let query = match query.is_empty() {
            true => String::new(),
            false => format!("?{}", query.join("&")),
        };
        format!(
            "{URL_SOURCE}+{}{query}#sha256:{}",
            crate::git::encode(&self.url),
            self.sha256
        )
    }

    /// The spec of a lockfile `source` written by [`UrlSpec::source`].
    pub fn parse_source(source: &str) -> Option<UrlSpec> {
        let rest = source.strip_prefix(URL_SOURCE)?.strip_prefix('+')?;
        let (rest, hash) = rest.rsplit_once('#')?;
        let sha256 = hash.strip_prefix("sha256:")?;
        if !is_sha256(sha256) {
            return None;
        }
        let (url, query) = rest.split_once('?').unwrap_or((rest, ""));
        let mut spec = UrlSpec {
            url: crate::git::decode(url)?,
            sha256: sha256.to_string(),
            strip: 0,
            subdir: None,
        };
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            match pair.split_once('=')? {
                ("strip", n) => spec.strip = n.parse().ok()?,
                ("path", dir) => spec.subdir = Some(crate::git::decode(dir)?),
                _ => return None,
            }
        }
        Some(spec)
    }

    /// How the spec reads in messages: `https://…/x.tar.gz (sha256 0123abcd…), path x`.
    pub fn describe(&self) -> String {
        let strip = match self.strip {
            0 => String::new(),
            n => format!(", strip {n}"),
        };
        let path = (self.subdir.as_ref())
            .map(|d| format!(", path {d}"))
            .unwrap_or_default();
        format!(
            "{} (sha256 {}…){strip}{path}",
            self.url.escape_default(),
            &self.sha256[..12]
        )
    }
}

/// Whether `s` is a SHA-256 in lowercase hex.
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Dependency {
    /// The url source, if this is a url dependency.
    pub fn url_spec(&self) -> Option<UrlSpec> {
        let Dependency::Detailed(d) = self else {
            return None;
        };
        Some(UrlSpec {
            url: d.url.clone()?,
            sha256: d.sha256.clone().unwrap_or_default(),
            strip: d.strip.unwrap_or(0),
            subdir: d.path.clone(),
        })
    }
}

/// `Err` unless `url` may be downloaded: `https`, or `http` to this machine; no credentials, and
/// nothing [`crate::source_url`] refuses when it compares a URL (an `allowNative` prefix matches
/// it as written).
pub fn check_url(url: &str) -> Result<(), String> {
    // The comparison's checks first: they say what is wrong (credentials, an escape) where the
    // download's would only say the URL can't be fetched.
    let as_https = match url.strip_prefix("http://") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    };
    if as_https.starts_with("https://") {
        crate::source_url::check_downloadable(&as_https)?;
    }
    velt_toolchain::install::check_download_url(url)
}

/// The url fields of dependency `name`: `sha256` and `strip` only with `url`, `sha256` required
/// and a SHA-256, `url` not with `git`, and `path` a directory inside the archive.
pub(crate) fn check_fields(name: &str, d: &DetailedDependency) -> Result<(), String> {
    let named = |e: String| format!("dependency `{name}`: {e}");
    let Some(url) = &d.url else {
        return match (&d.sha256, &d.strip) {
            (Some(_), _) => Err(named("`sha256` needs a `url`".into())),
            (_, Some(_)) => Err(named("`strip` needs a `url`".into())),
            _ => Ok(()),
        };
    };
    if d.git.is_some() {
        return Err(named(
            "`url` and `git` both name where it comes from; keep one".into(),
        ));
    }
    if d.branch.is_some() || d.tag.is_some() || d.rev.is_some() {
        return Err(named(
            "`branch`, `tag` and `rev` are for `git`, not `url`".into(),
        ));
    }
    check_url(url).map_err(named)?;
    match &d.sha256 {
        None => {
            return Err(named(format!(
                "a `url` needs its `sha256`, so the archive can't change under the lock (`velt add {url}` writes it)"
            )))
        }
        Some(h) if !is_sha256(h) => {
            return Err(named(format!(
                "`sha256` must be 64 lowercase hex digits, not `{h}`"
            )))
        }
        Some(_) => {}
    }
    if let Some(path) = &d.path {
        if !is_inner_path(path) {
            return Err(named(format!(
                "with `url`, `path` is a `/`-separated directory inside the archive, not `{path}`"
            )));
        }
    }
    Ok(())
}

/// A plain relative directory on every OS: no root, drive, `.`, `..` or backslash.
fn is_inner_path(path: &str) -> bool {
    path.split('/')
        .all(|s| !s.is_empty() && s != "." && s != ".." && !s.contains(['\\', ':']))
}

/// `<cache>/url/<sha256>`: the archive's raw tree.
fn raw_dir(loc: &Locations, sha256: &str) -> PathBuf {
    loc.cache.join(URL_SOURCE).join(sha256)
}

/// The package directory of `spec` and its tree checksum, downloading and extracting the archive
/// unless the cache has it. With `checksum` (the lock's), the tree must hash to it.
pub fn checkout(
    loc: &Locations,
    spec: &UrlSpec,
    checksum: Option<&str>,
) -> Result<(PathBuf, String), String> {
    let raw = raw_dir(loc, &spec.sha256);
    if !raw.is_dir() {
        download(spec, &raw)?;
    }
    let dir = choose(&raw, spec)?;
    let actual = crate::git::tree_checksum(&dir)?;
    if let Some(expected) = checksum {
        if actual != expected {
            return Err(format!(
                "checksum mismatch for {}: velt.lock.json has {expected}, the extracted tree hashes to {actual} (delete `{}` to download it again)",
                spec.describe(),
                raw.display()
            ));
        }
    }
    Ok((dir, actual))
}

/// The bytes at `url` (at most [`MAX_ARCHIVE`]).
fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    check_url(url)?;
    velt_toolchain::install::download(url, MAX_ARCHIVE)?
        .ok_or_else(|| format!("{url} doesn't exist (HTTP 404)"))
}

/// The entry `velt add <url>` writes: the archive is downloaded once, its SHA-256 computed and
/// the archive extracted into the cache, and the package found in it: at `path` (inside the
/// archive's one top directory when the archive has one and its root has no `package.velt`,
/// `strip: 1`). Returns the package's name and the spec.
pub fn entry_for(
    loc: &Locations,
    url: &str,
    path: Option<&str>,
) -> Result<(String, UrlSpec), String> {
    if let Some(p) = path {
        if !is_inner_path(p) {
            return Err(format!(
                "`--path` with a URL is a `/`-separated directory inside the archive, not `{p}`"
            ));
        }
    }
    let bytes = fetch_bytes(url)?;
    let sha256 = velt_toolchain::install::sha256_hex(&bytes);
    let raw = raw_dir(loc, &sha256);
    if !raw.is_dir() {
        unpack(&bytes, &raw, url)?;
    }
    let mut spec = UrlSpec {
        url: url.to_string(),
        sha256,
        strip: 0,
        subdir: path.map(String::from),
    };
    let has_manifest = |spec: &UrlSpec| {
        choose(&raw, spec).is_ok_and(|d| d.join(crate::manifest::MANIFEST_FILE).is_file())
    };
    if !has_manifest(&spec) {
        spec.strip = 1;
        if !has_manifest(&spec) {
            let at = path.map_or("its root".to_string(), |p| format!("`{p}`"));
            return Err(format!(
                "the archive at {url} has no `package.velt` at {at}, nor inside one top directory there: name the package's directory with `--path`"
            ));
        }
    }
    let dir = choose(&raw, &spec)?;
    let manifest = crate::manifest::Manifest::from_dir(&dir)?;
    Ok((manifest.package.name, spec))
}

/// Extract `bytes` (from `url`) into `raw`; a concurrent install's equal tree is taken.
fn unpack(bytes: &[u8], raw: &Path, url: &str) -> Result<(), String> {
    match crate::extract::extract(bytes, raw) {
        Err(_) if raw.is_dir() => Ok(()),
        result => result.map_err(|e| format!("the archive at {url} can't be used: {e}")),
    }
}

/// Download `spec`'s archive, check its hash, and extract it into `raw`.
fn download(spec: &UrlSpec, raw: &Path) -> Result<(), String> {
    let bytes = fetch_bytes(&spec.url)?;
    let actual = velt_toolchain::install::sha256_hex(&bytes);
    if actual != spec.sha256 {
        return Err(format!(
            "{} has SHA-256 {actual}, but the manifest asks for {}: the archive changed, or the URL names another one (forges' generated archives of a tag can change; use a release asset, or `git` with a commit)",
            spec.url, spec.sha256
        ));
    }
    unpack(&bytes, raw, &spec.url)
}

/// The archive's root after `strip` (what `path` is relative to), from the package directory
/// `dir` that [`checkout`] gave for `spec`.
pub fn archive_root(dir: &Path, spec: &UrlSpec) -> PathBuf {
    let depth = spec.subdir.as_ref().map_or(0, |d| d.split('/').count());
    let mut root = dir.to_path_buf();
    for _ in 0..depth {
        root.pop();
    }
    root
}

/// The directory `spec` names in the raw tree: `strip` levels down, each the only entry at its
/// level, then `path`; its links must stay inside it.
fn choose(raw: &Path, spec: &UrlSpec) -> Result<PathBuf, String> {
    let named = |e: String| format!("the archive at {}: {e}", spec.url);
    let mut dir = raw.to_path_buf();
    for level in 1..=spec.strip {
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| named(format!("cannot read `{}`: {e}", dir.display())))?
            .filter_map(Result::ok)
            .collect();
        match entries.as_slice() {
            [one] if one.file_type().is_ok_and(|t| t.is_dir()) => dir = one.path(),
            _ => {
                return Err(named(format!(
                    "`strip: {}` needs one directory at level {level}, but it has {} entries there",
                    spec.strip,
                    entries.len()
                )))
            }
        }
    }
    if let Some(sub) = &spec.subdir {
        dir = dir.join(sub);
    }
    let real = dir.canonicalize().map_err(|_| {
        named(format!(
            "it has no directory `{}`",
            spec.subdir.as_deref().unwrap_or(".")
        ))
    })?;
    let raw_real = raw.canonicalize().map_err(|e| named(e.to_string()))?;
    if !real.starts_with(&raw_real) || !real.is_dir() {
        return Err(named(
            "`path` leads out of the archive (through a link)".into(),
        ));
    }
    crate::git::check_links(&dir).map_err(|e| {
        named(format!(
            "the part `strip` and `path` choose {}",
            e.replace("out of the repository", "out of that part")
        ))
    })?;
    Ok(dir)
}

#[cfg(test)]
mod tests;
