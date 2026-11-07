//! What the cache knows about an archive, beside its raw tree `<cache>/url/<sha256>/`, in
//! `<cache>/url/<sha256>.verified`: the raw tree's checksum when it was extracted, and each URL
//! whose bytes were downloaded and hashed to `<sha256>`.
//!
//! The raw tree is keyed by hash alone, so a hit must not stand for a URL that never served
//! those bytes: `allowNative` matches a url package by the URL as written, and an archive cached
//! from one URL would otherwise take the trust meant for another (two dependencies naming one
//! hash, one of them from the URL the root allows). A URL not recorded is downloaded and hashed
//! once before the hit counts. And a raw tree changed after extraction must not reach a new lock:
//! with no lock to check against, the tree is read in full and compared with the checksum taken
//! when it was extracted.

use std::path::{Path, PathBuf};

use super::UrlSpec;

/// The record beside the raw tree of `sha256` under `url_dir` (`<cache>/url`).
pub(super) struct Verified {
    path: PathBuf,
    /// The raw tree's [`crate::git::tree_checksum`] when it was extracted.
    pub tree: String,
    /// URLs whose bytes hashed to the archive's SHA-256.
    pub urls: Vec<String>,
    /// The raw tree's symbolic links (relative), for the notes on Windows.
    pub links: Vec<String>,
}

impl Verified {
    pub fn path_for(raw: &Path) -> PathBuf {
        let mut name = raw.file_name().unwrap_or_default().to_os_string();
        name.push(".verified");
        raw.with_file_name(name)
    }

    /// The record of `raw`, if it has one that reads.
    pub fn read(raw: &Path) -> Option<Verified> {
        let path = Verified::path_for(raw);
        let text = std::fs::read_to_string(&path).ok()?;
        let mut tree = None;
        let (mut urls, mut links) = (vec![], vec![]);
        for line in text.lines() {
            match line.split_once(' ')? {
                ("tree", sum) => tree = Some(sum.to_string()),
                ("url", url) => urls.push(url.to_string()),
                ("link", link) => links.push(link.to_string()),
                _ => return None,
            }
        }
        Some(Verified {
            path,
            tree: tree?,
            urls,
            links,
        })
    }

    /// A new record for the raw tree `raw` from `url`, extracted at `staged` (which is renamed to
    /// `raw` once the record is written, so a raw tree never lacks one), with the archive's
    /// symbolic links `links` (from the extractor: on Windows they are written as files).
    pub fn new(
        raw: &Path,
        staged: &Path,
        url: &str,
        links: Vec<String>,
    ) -> Result<Verified, String> {
        Ok(Verified {
            path: Verified::path_for(raw),
            tree: crate::git::tree_checksum(staged)?,
            urls: vec![url.to_string()],
            links,
        })
    }

    pub fn knows(&self, url: &str) -> bool {
        self.urls.iter().any(|u| u == url)
    }

    /// Record `url` too and write the record. Written whole to a temporary file and renamed, so
    /// a reader never sees half of one.
    pub fn add_and_write(&mut self, url: &str) -> Result<(), String> {
        if !self.knows(url) {
            self.urls.push(url.to_string());
        }
        let mut text = format!("tree {}\n", self.tree);
        for url in &self.urls {
            text.push_str(&format!("url {url}\n"));
        }
        for link in &self.links {
            text.push_str(&format!("link {link}\n"));
        }
        let tmp = self
            .path
            .with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &self.path))
            .map_err(|e| format!("cannot write `{}`: {e}", self.path.display()))
    }

    /// `Err` unless the raw tree at `raw` still hashes to the checksum taken at extraction.
    pub fn check_tree(&self, raw: &Path, spec: &UrlSpec) -> Result<(), String> {
        let now = crate::git::tree_checksum(raw)?;
        if now != self.tree {
            return Err(format!(
                "the cached archive {} was changed after it was extracted (it hashes to {now}, not {}): delete `{}` to download it again",
                spec.describe(),
                self.tree,
                raw.display()
            ));
        }
        Ok(())
    }
}
