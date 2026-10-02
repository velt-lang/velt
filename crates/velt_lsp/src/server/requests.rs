//! Request dispatch: each handler runs isolated (a panic becomes an `InternalError` response) and
//! answers from the document's up-to-date analysis.

use std::panic::{catch_unwind, AssertUnwindSafe};

use lsp_server::{ErrorCode, Request, Response};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentHighlightRequest, DocumentSymbolRequest, Formatting,
    GotoDefinition, HoverRequest, InlayHintRequest, References, Rename, Request as LspRequest,
    SemanticTokensFullDeltaRequest, SemanticTokensFullRequest, SemanticTokensRangeRequest,
    SignatureHelpRequest, WorkspaceSymbolRequest,
};
use lsp_types::{
    CodeActionKind, CodeActionOptions, CodeActionProviderCapability, CompletionOptions,
    GotoDefinitionResponse, HoverProviderCapability, Location, OneOf, SemanticTokensFullOptions,
    SemanticTokensOptions, SemanticTokensServerCapabilities, ServerCapabilities,
    SignatureHelpOptions, TextDocumentPositionParams, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit, Url,
    WorkspaceEdit,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use velt_common::Span;

use super::Server;
use crate::index::scope;
use crate::line_index::LineIndex;
use crate::{
    completion, definition, highlight, hover, inlay_hints, manifest, references, sema_query,
    semantic_tokens, signature_help, symbols,
};

/// What the server supports.
pub fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                ..Default::default()
            },
        )),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        definition_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".into(), "<".into()]),
            ..Default::default()
        }),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![
                CodeActionKind::QUICKFIX,
                CodeActionKind::SOURCE_FIX_ALL,
            ]),
            ..Default::default()
        })),
        inlay_hint_provider: Some(OneOf::Left(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        }),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: semantic_tokens::legend(),
                full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                range: Some(true),
                ..Default::default()
            },
        )),
        document_highlight_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        ..Default::default()
    }
}

/// Why a request failed (besides panics).
enum RequestError {
    MethodNotFound,
    InvalidParams(String),
}

impl Server<'_> {
    pub(super) fn request(&mut self, req: Request) {
        let Request { id, method, params } = req;
        let result = catch_unwind(AssertUnwindSafe(|| self.dispatch(&method, params)));
        let response = match result {
            Ok(Ok(value)) => Response::new_ok(id, value),
            Ok(Err(RequestError::MethodNotFound)) => Response::new_err(
                id,
                ErrorCode::MethodNotFound as i32,
                format!("unsupported request `{method}`"),
            ),
            Ok(Err(RequestError::InvalidParams(msg))) => {
                Response::new_err(id, ErrorCode::InvalidParams as i32, msg)
            }
            Err(_) => Response::new_err(
                id,
                ErrorCode::InternalError as i32,
                format!("internal error while handling `{method}`"),
            ),
        };
        self.send(response.into());
    }

    fn dispatch(&mut self, method: &str, params: Value) -> Result<Value, RequestError> {
        match method {
            Formatting::METHOD => {
                let p: lsp_types::DocumentFormattingParams = parse(params)?;
                Ok(json(self.formatting(&p.text_document.uri)))
            }
            DocumentSymbolRequest::METHOD => {
                let p: lsp_types::DocumentSymbolParams = parse(params)?;
                let symbols = self
                    .analysis(&p.text_document.uri)
                    .map(symbols::document_symbols);
                Ok(json(symbols))
            }
            GotoDefinition::METHOD => {
                let p: lsp_types::GotoDefinitionParams = parse(params)?;
                Ok(json(self.definition(&p.text_document_position_params)))
            }
            HoverRequest::METHOD => {
                let p: lsp_types::HoverParams = parse(params)?;
                let pos = &p.text_document_position_params;
                if let Some(text) = self.manifest_text(&pos.text_document.uri) {
                    let at = LineIndex::new(text).offset(pos.position);
                    return Ok(json(manifest::hover(text, at)));
                }
                let hover = self
                    .analysis(&pos.text_document.uri)
                    .and_then(|a| hover::hover(a, offset(a, pos)));
                Ok(json(hover))
            }
            Completion::METHOD => {
                let p: lsp_types::CompletionParams = parse(params)?;
                let pos = &p.text_document_position;
                // `<` triggers completion for JSX tags only, not after every comparison.
                let jsx_only = p
                    .context
                    .as_ref()
                    .and_then(|c| c.trigger_character.as_deref())
                    == Some("<");
                if let Some(text) = self.manifest_text(&pos.text_document.uri) {
                    let at = LineIndex::new(text).offset(pos.position);
                    let items = if jsx_only {
                        vec![]
                    } else {
                        manifest::completion(text, at)
                    };
                    return Ok(json(Some(items)));
                }
                let items = self
                    .analysis(&pos.text_document.uri)
                    .map(|a| completion::complete(a, offset(a, pos), jsx_only));
                Ok(json(items))
            }
            References::METHOD => {
                let p: lsp_types::ReferenceParams = parse(params)?;
                Ok(json(self.references(&p)))
            }
            Rename::METHOD => {
                let p: lsp_types::RenameParams = parse(params)?;
                self.rename(&p).map(json)
            }
            _ => self.dispatch_more(method, params),
        }
    }

    /// Editing aids: code actions, inlay hints, signature help, semantic tokens, highlights and
    /// workspace symbols.
    fn dispatch_more(&mut self, method: &str, params: Value) -> Result<Value, RequestError> {
        match method {
            CodeActionRequest::METHOD => {
                let p: lsp_types::CodeActionParams = parse(params)?;
                Ok(json(self.code_actions(&p)))
            }
            InlayHintRequest::METHOD => {
                let p: lsp_types::InlayHintParams = parse(params)?;
                let hints = self.analysis(&p.text_document.uri).map(|a| {
                    let index = LineIndex::new(a.text());
                    let (lo, hi) = (index.offset(p.range.start), index.offset(p.range.end));
                    inlay_hints::inlay_hints(a, lo, hi)
                });
                Ok(json(hints))
            }
            SignatureHelpRequest::METHOD => {
                let p: lsp_types::SignatureHelpParams = parse(params)?;
                let pos = &p.text_document_position_params;
                let help = self
                    .analysis(&pos.text_document.uri)
                    .and_then(|a| signature_help::signature_help(a, offset(a, pos)));
                Ok(json(help))
            }
            SemanticTokensFullRequest::METHOD => {
                Ok(json(self.semantic_tokens_full(&parse(params)?)))
            }
            SemanticTokensFullDeltaRequest::METHOD => {
                Ok(json(self.semantic_tokens_delta(&parse(params)?)))
            }
            SemanticTokensRangeRequest::METHOD => {
                Ok(json(self.semantic_tokens_range(&parse(params)?)))
            }
            DocumentHighlightRequest::METHOD => {
                let p: lsp_types::DocumentHighlightParams = parse(params)?;
                let pos = &p.text_document_position_params;
                let highlights = self
                    .analysis(&pos.text_document.uri)
                    .and_then(|a| highlight::highlights(a, offset(a, pos)));
                Ok(json(highlights))
            }
            WorkspaceSymbolRequest::METHOD => {
                let p: lsp_types::WorkspaceSymbolParams = parse(params)?;
                Ok(json(self.workspace_symbols(&p.query)))
            }
            _ => Err(RequestError::MethodNotFound),
        }
    }

    /// The editor location of `span` (in any loaded file).
    fn location(&self, analysis: &crate::analysis::Analysis, span: Span) -> Option<Location> {
        let file = analysis.sm.get(span.file);
        let uri = self.uri_of(&file.path)?;
        let range = LineIndex::new(&file.src).range(span.lo, span.hi);
        Some(Location::new(uri, range))
    }

    fn references(&mut self, p: &lsp_types::ReferenceParams) -> Option<Vec<Location>> {
        let pos = &p.text_document_position;
        self.analysis(&pos.text_document.uri)?;
        let analysis = self.analyses.get(&pos.text_document.uri)?;
        let spans = references::references(
            analysis,
            offset(analysis, pos),
            p.context.include_declaration,
        );
        Some(
            spans
                .into_iter()
                .filter_map(|s| self.location(analysis, s))
                .collect(),
        )
    }

    fn rename(
        &mut self,
        p: &lsp_types::RenameParams,
    ) -> Result<Option<WorkspaceEdit>, RequestError> {
        let pos = &p.text_document_position;
        if self.analysis(&pos.text_document.uri).is_none() {
            return Ok(None);
        }
        let Some(analysis) = self.analyses.get(&pos.text_document.uri) else {
            return Ok(None);
        };
        let spans = references::rename(analysis, offset(analysis, pos), &p.new_name)
            .map_err(RequestError::InvalidParams)?;
        let mut changes: std::collections::HashMap<Url, Vec<TextEdit>> = Default::default();
        for s in spans {
            if let Some(loc) = self.location(analysis, s) {
                let edit = TextEdit::new(loc.range, p.new_name.clone());
                changes.entry(loc.uri).or_default().push(edit);
            }
        }
        Ok(Some(WorkspaceEdit::new(changes)))
    }

    /// One edit replacing the whole document, or none (already formatted / does not parse).
    fn formatting(&self, uri: &Url) -> Option<Vec<TextEdit>> {
        let doc = self.docs.get(uri)?;
        let formatted = velt_fmt::format_source(&doc.text).ok()?;
        if formatted == doc.text {
            return Some(vec![]);
        }
        let range = LineIndex::new(&doc.text).full_range();
        Some(vec![TextEdit::new(range, formatted)])
    }

    fn definition(&mut self, pos: &TextDocumentPositionParams) -> Option<GotoDefinitionResponse> {
        self.analysis(&pos.text_document.uri)?;
        let analysis = self.analyses.get(&pos.text_document.uri)?;
        let at = offset(analysis, pos);
        let span = match sema_query::def_at(analysis, at) {
            Some(def) => def.span,
            None => {
                let info = scope::at_offset(analysis, at);
                definition::resolve(analysis, &info)?.name_span
            }
        };
        let loc = self.location(analysis, span)?;
        Some(GotoDefinitionResponse::Scalar(loc))
    }
}

fn offset(analysis: &crate::analysis::Analysis, pos: &TextDocumentPositionParams) -> u32 {
    LineIndex::new(analysis.text()).offset(pos.position)
}

fn parse<P: DeserializeOwned>(params: Value) -> Result<P, RequestError> {
    serde_json::from_value(params).map_err(|e| RequestError::InvalidParams(e.to_string()))
}

fn json(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}
