//! The server: handshake, main loop, document notifications and the analysis cache.
//!
//! Diagnostics are debounced: an edit schedules its document (and every other open document, which
//! may import it) for analysis [`DEBOUNCE`] later; further edits push the deadline back. A request
//! on a document with a pending analysis runs it first, so answers always match the latest text.
//! Closing a `package.vlt`, a manifest changing on disk and a folder appearing or disappearing
//! schedule every open document too: they decide which files are in `tsCompat` folders.

mod completion;
mod features;
mod requests;
mod tokens;

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    DidSaveTextDocument, Notification as _, PublishDiagnostics,
};
use lsp_types::request::{RegisterCapability, Request as _};
use lsp_types::{FileChangeType, PublishDiagnosticsParams, Url};

use crate::analysis::{self, Analysis};
use crate::disk_index::DiskIndex;
use crate::documents::{self, Documents};
use crate::imports::ImportHelp;
use crate::registry::RegistryData;
use crate::{diagnostics, manifest, ts_compat, workspace_symbols, ProgramLoader};

/// Quiet time after an edit before the document is re-analyzed.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// How often an open manifest is re-checked while registry data is being fetched.
const REGISTRY_POLL: Duration = Duration::from_millis(250);

/// Handshake with the client, then serve until `exit` or disconnection.
pub fn run(connection: &Connection, loader: &dyn ProgramLoader) -> Result<(), String> {
    let (id, params) = connection
        .initialize_start()
        .map_err(|e| format!("language server handshake failed: {e}"))?;
    let result = serde_json::json!({
        "capabilities": requests::capabilities(),
        "serverInfo": { "name": "velt-lsp", "version": env!("CARGO_PKG_VERSION") },
    });
    connection
        .initialize_finish(id, result)
        .map_err(|e| format!("language server handshake failed: {e}"))?;
    watch_files(connection, &params);
    Server {
        connection,
        loader,
        docs: Documents::default(),
        analyses: HashMap::new(),
        pending: HashMap::new(),
        roots: workspace_symbols::roots_from_init(&params),
        sent_tokens: HashMap::new(),
        next_result_id: 0,
        disk_symbols: DiskIndex::default(),
        registry: RegistryData::default(),
        ts_folders: Default::default(),
        imports: Default::default(),
    }
    .main_loop()
}

struct Server<'a> {
    connection: &'a Connection,
    loader: &'a dyn ProgramLoader,
    docs: Documents,
    /// Latest analysis per open document (stale while the document is in `pending`).
    analyses: HashMap<Url, Analysis>,
    /// Documents awaiting (re-)analysis, with the time it is due.
    pending: HashMap<Url, Instant>,
    /// Workspace folders (searched by workspace symbols).
    roots: Vec<PathBuf>,
    /// The semantic tokens last sent per document, with their result id (for deltas).
    sent_tokens: HashMap<Url, (String, Vec<lsp_types::SemanticToken>)>,
    /// The last semantic tokens result id handed out.
    next_result_id: u64,
    /// Symbols of the workspace folders' files.
    disk_symbols: DiskIndex,
    /// Package indexes and searches for `package.vlt`, fetched in the background.
    registry: RegistryData,
    /// The packages' `tsCompat` folders (cleared when folders appear or disappear on disk).
    ts_folders: ts_compat::FolderCache,
    /// Parsed exports of modules outside the analyzed programs (import completion, auto-import).
    imports: ImportHelp,
}

/// Id of the request registering the file watcher.
const WATCH_REQUEST: &str = "velt-watch";

/// Ask the client to report changes of source files (`.vlt`, `.ts`, `.tsx`) if it can (dynamic
/// registration of `workspace/didChangeWatchedFiles`). The index relies on the events once the
/// client answers the request successfully.
fn watch_files(connection: &Connection, init: &serde_json::Value) {
    let supported = init["capabilities"]["workspace"]["didChangeWatchedFiles"]
        ["dynamicRegistration"]
        .as_bool()
        .unwrap_or(false);
    if supported {
        let params = serde_json::json!({ "registrations": [{
            "id": "velt-source-files",
            "method": DidChangeWatchedFiles::METHOD,
            // Source files, and creations and deletions of anything (folders are reported by
            // their own path: kind 5 = create + delete).
            "registerOptions": { "watchers": [
                { "globPattern": "**/*.{vlt,ts,tsx}" },
                { "globPattern": "**/*", "kind": 5 },
            ] },
        }] });
        let id = RequestId::from(WATCH_REQUEST.to_string());
        let req = Request::new(id, RegisterCapability::METHOD.into(), params);
        let _ = connection.sender.send(req.into());
    }
}

/// Whether a watched-file event for `path` can change which files are in `tsCompat` folders, or
/// which folders an open manifest misses: a manifest saved elsewhere, or a folder created or
/// deleted in a package (a deleted path is gone, so anything but a source file counts). Paths
/// the source walk never enters (`.git/`, `node_modules/`, `target/`, …) don't, which keeps a
/// busy `.git/` from re-analyzing the open documents.
pub(crate) fn affects_packages(path: &Path, created: bool, deleted: bool) -> bool {
    if manifest::is_manifest(path) {
        return true;
    }
    let Some(root) = path.parent().and_then(vpm::manifest::find_package_root) else {
        return false;
    };
    let skipped = path.strip_prefix(&root).map_or(true, |rel| {
        rel.components().any(|c| {
            let name = c.as_os_str().to_string_lossy();
            name.starts_with('.') || vpm::sources::SKIPPED_DIRS.contains(&name.as_ref())
        })
    });
    !skipped && ((created && path.is_dir()) || (deleted && !vpm::sources::is_source_file(path)))
}

impl Server<'_> {
    fn main_loop(&mut self) -> Result<(), String> {
        loop {
            let next = self.pending.values().min().copied();
            let received = match next {
                Some(deadline) => match self.connection.receiver.recv_deadline(deadline) {
                    Ok(msg) => Some(msg),
                    Err(e) if e.is_timeout() => None,
                    Err(_) => return Ok(()),
                },
                None => match self.connection.receiver.recv() {
                    Ok(msg) => Some(msg),
                    Err(_) => return Ok(()),
                },
            };
            match received {
                None => self.flush_due(),
                Some(Message::Request(req)) => {
                    let shutdown = self
                        .connection
                        .handle_shutdown(&req)
                        .map_err(|e| format!("language server shutdown failed: {e}"))?;
                    if shutdown {
                        return Ok(());
                    }
                    self.request(req);
                }
                Some(Message::Notification(n)) if n.method == "exit" => return Ok(()),
                Some(Message::Notification(n)) => self.notification(n),
                Some(Message::Response(r)) => {
                    // The file watcher counts once the client accepted it.
                    if r.id == RequestId::from(WATCH_REQUEST.to_string())
                        && r.response_result.is_ok()
                    {
                        self.disk_symbols.watched = true;
                    }
                }
            }
        }
    }

    fn notification(&mut self, n: Notification) {
        let method = n.method.clone();
        let handled = catch_unwind(AssertUnwindSafe(|| self.document_notification(n)));
        match handled {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("velt-lsp: bad `{method}` notification: {e}"),
            Err(_) => eprintln!("velt-lsp: internal error while handling `{method}`"),
        }
    }

    fn document_notification(&mut self, n: Notification) -> Result<(), serde_json::Error> {
        match n.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let p: lsp_types::DidOpenTextDocumentParams = serde_json::from_value(n.params)?;
                let doc = p.text_document;
                self.docs.open(doc.uri.clone(), doc.text, doc.version);
                self.schedule(&doc.uri, Duration::ZERO);
            }
            DidChangeTextDocument::METHOD => {
                let p: lsp_types::DidChangeTextDocumentParams = serde_json::from_value(n.params)?;
                // Full sync: the last change carries the whole text.
                if let Some(change) = p.content_changes.into_iter().last() {
                    let doc = p.text_document;
                    self.docs.change(&doc.uri, change.text, doc.version);
                    self.schedule(&doc.uri, DEBOUNCE);
                }
            }
            DidSaveTextDocument::METHOD => {
                let p: lsp_types::DidSaveTextDocumentParams = serde_json::from_value(n.params)?;
                self.schedule(&p.text_document.uri, Duration::ZERO);
            }
            DidChangeWatchedFiles::METHOD => {
                let p: lsp_types::DidChangeWatchedFilesParams = serde_json::from_value(n.params)?;
                let mut packages_changed = false;
                for change in p.changes {
                    let path = documents::uri_to_path(&change.uri);
                    let deleted = change.typ == FileChangeType::DELETED;
                    self.disk_symbols.changed(&self.roots, &path, deleted);
                    self.imports.forget(&path);
                    let created = change.typ == FileChangeType::CREATED;
                    packages_changed |= affects_packages(&path, created, deleted);
                }
                if packages_changed {
                    self.ts_folders.clear();
                    self.schedule_open();
                }
            }
            DidCloseTextDocument::METHOD => {
                let p: lsp_types::DidCloseTextDocumentParams = serde_json::from_value(n.params)?;
                let uri = p.text_document.uri;
                self.docs.close(&uri);
                self.analyses.remove(&uri);
                self.pending.remove(&uri);
                self.sent_tokens.remove(&uri);
                let closed_manifest = manifest::is_manifest(&documents::uri_to_path(&uri));
                self.publish(uri, vec![], None);
                if closed_manifest {
                    // Its unsaved text no longer counts: the file on disk does.
                    self.schedule_open();
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Schedule `uri` for analysis after `delay`, and every other open document too (they may
    /// import it); an already scheduled deadline moves later, never earlier than `delay` allows.
    fn schedule(&mut self, uri: &Url, delay: Duration) {
        let due = Instant::now() + delay;
        self.pending.insert(uri.clone(), due);
        for other in self.docs.uris() {
            self.pending.entry(other).or_insert(due + DEBOUNCE);
        }
    }

    /// Schedule every open document for analysis after [`DEBOUNCE`] (a package's manifest or
    /// folders changed under them), keeping earlier deadlines.
    fn schedule_open(&mut self) {
        let due = Instant::now() + DEBOUNCE;
        for uri in self.docs.uris() {
            self.pending.entry(uri).or_insert(due);
        }
    }

    /// Analyze and publish every document whose deadline passed.
    fn flush_due(&mut self) {
        let now = Instant::now();
        let due: Vec<Url> = self
            .pending
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(uri, _)| uri.clone())
            .collect();
        for uri in due {
            let analyzed = catch_unwind(AssertUnwindSafe(|| self.refresh(&uri)));
            if analyzed.is_err() {
                self.pending.remove(&uri);
                eprintln!("velt-lsp: internal error while analyzing {uri}");
            }
        }
    }

    /// Re-analyze `uri` now and publish its diagnostics.
    fn refresh(&mut self, uri: &Url) {
        self.pending.remove(uri);
        let Some(doc) = self.docs.get(uri) else {
            return;
        };
        let (path, version) = (doc.path.clone(), doc.version);
        if manifest::is_manifest(&path) {
            // Data, not a program: the reader's and the registry's diagnostics, no analysis.
            let diags = manifest::diagnostics(&doc.text, &self.registry, path.parent());
            self.analyses.remove(uri);
            self.publish(uri.clone(), diags, Some(version));
            if self.registry.busy() {
                // Registry data is on its way: look again when it may have arrived.
                self.pending
                    .insert(uri.clone(), Instant::now() + REGISTRY_POLL);
            }
            return;
        }
        let overlay = self.docs.overlay();
        let analysis = analysis::analyze(self.loader, &path, &overlay, &mut self.ts_folders);
        let diags = diagnostics::for_document(&analysis, &|p| self.uri_of(p));
        self.analyses.insert(uri.clone(), analysis);
        self.publish(uri.clone(), diags, Some(version));
    }

    /// The text of an open package manifest (which has no analysis).
    fn manifest_text(&self, uri: &Url) -> Option<&str> {
        let doc = self.docs.get(uri)?;
        manifest::is_manifest(&doc.path).then_some(doc.text.as_str())
    }

    /// The up-to-date analysis of an open document (analyzing it now if an edit is pending); none
    /// for a package manifest.
    fn analysis(&mut self, uri: &Url) -> Option<&Analysis> {
        if self.manifest_text(uri).is_some() {
            return None;
        }
        if self.pending.contains_key(uri) || !self.analyses.contains_key(uri) {
            self.refresh(uri);
        }
        self.analyses.get(uri)
    }

    /// URI for a file: the open document's own URI if it is open (clients match diagnostics and
    /// navigation by exact URI), else a `file:` URI.
    fn uri_of(&self, path: &Path) -> Option<Url> {
        self.docs
            .uris()
            .into_iter()
            .find(|uri| self.docs.get(uri).is_some_and(|d| d.path == path))
            .or_else(|| documents::path_to_uri(path))
    }

    fn publish(&self, uri: Url, diagnostics: Vec<lsp_types::Diagnostic>, version: Option<i32>) {
        let params = PublishDiagnosticsParams {
            uri,
            diagnostics,
            version,
        };
        self.send(Notification::new(PublishDiagnostics::METHOD.into(), params).into());
    }

    fn send(&self, msg: Message) {
        if self.connection.sender.send(msg).is_err() {
            eprintln!("velt-lsp: client connection closed");
        }
    }
}
