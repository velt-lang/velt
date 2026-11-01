//! Slow diagnostics ([`ProgramLoader::slow_diagnostics`](crate::ProgramLoader): the source
//! generators'): one job at a time, on a thread of its own, for a document's latest analysis
//! [`DELAY`] after it (later analyses push it back). What a job finds is published with the
//! document's other diagnostics when it is done, if the document is still at the version the job
//! ran on, and again with each analysis of that version. Requests never wait for a job.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use lsp_types::Url;
use velt_common::{Diagnostic, Diagnostics};

use super::Server;
use crate::diagnostics;

/// Quiet time after a document's analysis before its slow diagnostics run.
const DELAY: Duration = Duration::from_millis(300);

/// The slow diagnostics' jobs and findings.
#[derive(Default)]
pub(super) struct Slow {
    running: Option<Running>,
    /// Documents analyzed since their last job started, with when their job is due.
    wanted: HashMap<Url, Instant>,
    /// What the last finished job of each document found, and the version it ran on.
    found: HashMap<Url, (i32, Diagnostics)>,
}

/// The job running: its document, the document's version, and where its findings come.
struct Running {
    uri: Url,
    version: i32,
    done: Receiver<Diagnostics>,
}

impl Slow {
    /// What the last job found for `uri` if it ran on `version` (and nothing for another one).
    pub(super) fn found(&self, uri: &Url, version: i32) -> &[Diagnostic] {
        match self.found.get(uri) {
            Some((v, found)) if *v == version => found,
            _ => &[],
        }
    }

    /// Run a job for `uri`'s analysis after [`DELAY`].
    pub(super) fn want(&mut self, uri: &Url) {
        self.wanted.insert(uri.clone(), Instant::now() + DELAY);
    }

    /// `uri` was closed.
    pub(super) fn forget(&mut self, uri: &Url) {
        self.wanted.remove(uri);
        self.found.remove(uri);
    }

    /// When the next job is due (none while one runs).
    pub(super) fn next_due(&self) -> Option<Instant> {
        match self.running {
            Some(_) => None,
            None => self.wanted.values().min().copied(),
        }
    }

    /// Where the running job's findings come (a channel that never delivers without one).
    pub(super) fn done(&self) -> Receiver<Diagnostics> {
        (self.running.as_ref()).map_or_else(crossbeam_channel::never, |r| r.done.clone())
    }
}

impl Server<'_> {
    /// Start the job of the document whose job is due first, unless one runs.
    pub(super) fn start_slow(&mut self) {
        let now = Instant::now();
        while self.slow.running.is_none() {
            let due = (self.slow.wanted.iter())
                .filter(|(_, at)| **at <= now)
                .min_by_key(|(_, at)| **at)
                .map(|(uri, _)| uri.clone());
            let Some(uri) = due else {
                return;
            };
            self.slow.wanted.remove(&uri);
            // An edit is waiting to be analyzed: that analysis asks again.
            if self.pending.contains_key(&uri) {
                continue;
            }
            let (Some(doc), Some(analysis)) = (self.docs.get(&uri), self.analyses.get(&uri)) else {
                continue;
            };
            let (path, version) = (doc.path.clone(), doc.version);
            let job = catch_unwind(AssertUnwindSafe(|| {
                let root = analysis.root;
                (self.loader).slow_diagnostics(&path, &analysis.sm, &analysis.modules, root)
            }));
            let Ok(Some(job)) = job else {
                continue;
            };
            let (tx, rx) = crossbeam_channel::bounded(1);
            let spawned = std::thread::Builder::new()
                .name("velt-lsp-slow".into())
                .stack_size(crate::SERVER_STACK_BYTES)
                .spawn(move || {
                    // A crash in the job costs its findings, not the server.
                    let found = catch_unwind(AssertUnwindSafe(job)).unwrap_or_default();
                    let _ = tx.send(found);
                });
            if spawned.is_ok() {
                let done = rx;
                self.slow.running = Some(Running { uri, version, done });
            }
        }
    }

    /// The running job is done with `found`: publish it with its document's diagnostics if the
    /// document did not change since, then start the next job that is due.
    pub(super) fn finish_slow(&mut self, found: Diagnostics) {
        let Some(run) = self.slow.running.take() else {
            return;
        };
        let current = self.docs.get(&run.uri).map(|d| d.version);
        if current == Some(run.version) && !self.pending.contains_key(&run.uri) {
            if let Some(analysis) = self.analyses.get(&run.uri) {
                let diags = diagnostics::for_document(analysis, &found, &|p| self.uri_of(p));
                self.publish(run.uri.clone(), diags, Some(run.version));
            }
            self.slow.found.insert(run.uri, (run.version, found));
        }
        self.start_slow();
    }
}
