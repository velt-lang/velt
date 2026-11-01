//! Slow diagnostics (`ProgramLoader::slow_diagnostics`): published after the document's own,
//! dropped once the document changes, and never in the way of requests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use velt_common::{Diagnostic, Diagnostics, SourceMap, Span};
use velt_sema::SourceModule;

use super::client::{at, uri, Client};
use super::loader::TestLoader;
use crate::{LoadedProgram, ProgramLoader, SlowJob};

/// [`TestLoader`] whose slow job reports `slow!` at each `slow` in the document. A job tells the
/// test that it started (`started`), and ends when the test lets it (`go`).
struct SlowLoader {
    go: Arc<Mutex<Receiver<()>>>,
    started: Mutex<Sender<()>>,
}

impl ProgramLoader for SlowLoader {
    fn load(
        &self,
        root: &Path,
        overlay: &HashMap<PathBuf, String>,
        sm: &mut SourceMap,
        diags: &mut Diagnostics,
    ) -> Result<LoadedProgram, String> {
        TestLoader.load(root, overlay, sm, diags)
    }

    fn slow_diagnostics(
        &self,
        _: &Path,
        sm: &SourceMap,
        modules: &[SourceModule],
        root: usize,
    ) -> Option<SlowJob> {
        let file = modules[root].file;
        let text = sm.get(file).src.clone();
        let go = self.go.clone();
        let started = self.started.lock().unwrap().clone();
        Some(Box::new(move || {
            let _ = started.send(());
            let _ = go.lock().unwrap().recv();
            (text.match_indices("slow").map(|(i, _)| {
                let span = Span::new(file, i as u32, i as u32 + 4);
                Diagnostic::error("slow!", span)
            }))
            .collect()
        }))
    }
}

fn messages(params: &Value) -> Vec<String> {
    (params["diagnostics"].as_array().unwrap().iter())
        .map(|d| d["message"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn slow_diagnostics_follow_the_document_s_own() {
    let (go, go_rx) = channel();
    let (started_tx, started) = channel();
    let loader = SlowLoader {
        go: Arc::new(Mutex::new(go_rx)),
        started: Mutex::new(started_tx),
    };
    let mut client = Client::start_on(json!({ "capabilities": {} }), loader);
    let doc = uri("slow.vlt");
    client.open(&doc, "function main() {\n  const slow = 1;\n}\n");
    // The document's own diagnostics come at once; then its job starts.
    let first = client.diagnostics(&doc);
    assert_eq!(messages(&first), Vec::<String>::new());
    started.recv().unwrap();
    // While it runs, an edit is analyzed and a hover answered.
    let text = "function main() {\n  const fast = 1; // slow\n}\n";
    client.change(&doc, 2, text);
    let hover = client.request("textDocument/hover", at(&doc, 1, 9));
    assert!(hover.to_string().contains("fast"), "{hover}");
    let own = client.diagnostics(&doc);
    assert_eq!((own["version"].clone(), messages(&own)), (json!(2), vec![]));
    // The first job's findings are for a version that is gone: not published.
    go.send(()).unwrap();
    started.recv().unwrap();
    go.send(()).unwrap();
    let found = client.diagnostics(&doc);
    assert_eq!(
        (found["version"].clone(), messages(&found)),
        (json!(2), vec!["slow!".to_string()])
    );
    let range = &found["diagnostics"][0]["range"];
    assert_eq!(range["start"], json!({ "line": 1, "character": 21 }));
    // Changed again: the findings stay until the next job reports (no blinking), then go.
    client.change(&doc, 3, &text.replace("// slow", "// fast"));
    let own = client.diagnostics(&doc);
    assert_eq!(
        (own["version"].clone(), messages(&own)),
        (json!(3), vec!["slow!".to_string()])
    );
    started.recv().unwrap();
    go.send(()).unwrap();
    let found = client.diagnostics(&doc);
    assert_eq!(
        (found["version"].clone(), messages(&found)),
        (json!(3), vec![])
    );
    // Findings that no longer fit the text (past its end) are not shown.
    client.change(&doc, 4, text);
    let own = client.diagnostics(&doc);
    assert_eq!((own["version"].clone(), messages(&own)), (json!(4), vec![]));
    started.recv().unwrap();
    go.send(()).unwrap();
    let found = client.diagnostics(&doc);
    assert_eq!(messages(&found), ["slow!"]);
    client.change(&doc, 5, "function main() {}\n");
    let own = client.diagnostics(&doc);
    assert_eq!((own["version"].clone(), messages(&own)), (json!(5), vec![]));
    started.recv().unwrap();
    go.send(()).unwrap();
    client.diagnostics(&doc);
    client.shutdown();
}
