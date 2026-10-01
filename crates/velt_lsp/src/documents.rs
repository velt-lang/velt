//! The open documents (full-text sync) and URI ↔ path conversion.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::Url;

/// One open editor buffer.
pub struct Document {
    /// File path the URI stands for (non-`file:` URIs get a synthetic path).
    pub path: PathBuf,
    /// Current text.
    pub text: String,
    /// Client version of `text`.
    pub version: i32,
}

/// All open documents by URI.
#[derive(Default)]
pub struct Documents {
    docs: HashMap<Url, Document>,
}

impl Documents {
    /// Open (or reopen) `uri` with `text`.
    pub fn open(&mut self, uri: Url, text: String, version: i32) {
        let path = uri_to_path(&uri);
        self.docs.insert(
            uri,
            Document {
                path,
                text,
                version,
            },
        );
    }

    /// Replace the text of an open document; ignored for unknown URIs.
    pub fn change(&mut self, uri: &Url, text: String, version: i32) {
        if let Some(doc) = self.docs.get_mut(uri) {
            doc.text = text;
            doc.version = version;
        }
    }

    /// Forget `uri`.
    pub fn close(&mut self, uri: &Url) {
        self.docs.remove(uri);
    }

    /// The document at `uri`, if open.
    pub fn get(&self, uri: &Url) -> Option<&Document> {
        self.docs.get(uri)
    }

    /// URIs of all open documents.
    pub fn uris(&self) -> Vec<Url> {
        self.docs.keys().cloned().collect()
    }

    /// Path → text of every open document, for the loader's overlay.
    pub fn overlay(&self) -> HashMap<PathBuf, String> {
        self.docs
            .values()
            .map(|d| (d.path.clone(), d.text.clone()))
            .collect()
    }
}

/// The file path of a `file:` URI; other schemes (e.g. `untitled:`) map to a path that does not
/// exist, so the loader reads them from the overlay only.
pub fn uri_to_path(uri: &Url) -> PathBuf {
    uri.to_file_path()
        .unwrap_or_else(|_| PathBuf::from(format!("{}.vlt", uri.as_str().replace(':', "_"))))
}

/// The `file:` URI of `path` (made absolute first; `None` if that fails).
pub fn path_to_uri(path: &Path) -> Option<Url> {
    let absolute = std::path::absolute(path).ok()?;
    Url::from_file_path(absolute).ok()
}
