//! The server: handshake, main loop, document notifications and the analysis cache.
//!
//! Diagnostics are debounced: an edit schedules its document (and every other open document, which
//! may import it) for analysis [`DEBOUNCE`] later; further edits push the deadline back. A request
//! on a document with a pending analysis runs it first, so answers always match the latest text.

mod features;
mod requests;

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lsp_server::{Connection, Message, Notification};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::{PublishDiagnosticsParams, Url};

use crate::analysis::{self, Analysis};
use crate::documents::{self, Documents};
use crate::{diagnostics, workspace_symbols, ProgramLoader};

/// Quiet time after an edit before the document is re-analyzed.
const DEBOUNCE: Duration = Duration::from_millis(150);

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
    Server {
        connection,
        loader,
        docs: Documents::default(),
        analyses: HashMap::new(),
        pending: HashMap::new(),
        roots: workspace_symbols::roots_from_init(&params),
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
                Some(Message::Response(_)) => {}
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
            DidCloseTextDocument::METHOD => {
                let p: lsp_types::DidCloseTextDocumentParams = serde_json::from_value(n.params)?;
                let uri = p.text_document.uri;
                self.docs.close(&uri);
                self.analyses.remove(&uri);
                self.pending.remove(&uri);
                self.publish(uri, vec![], None);
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
        let analysis = analysis::analyze(self.loader, &path, &self.docs.overlay());
        let diags = diagnostics::for_document(&analysis, &|p| self.uri_of(p));
        self.analyses.insert(uri.clone(), analysis);
        self.publish(uri.clone(), diags, Some(version));
    }

    /// The up-to-date analysis of an open document (analyzing it now if an edit is pending).
    fn analysis(&mut self, uri: &Url) -> Option<&Analysis> {
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
