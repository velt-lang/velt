//! Semantic token requests: `full` (remembered per document under a result id), `full/delta`
//! (one edit against the tokens sent with the client's previous result id, or everything when
//! the server no longer has them) and `range`.

use lsp_types::{
    SemanticTokens, SemanticTokensDelta, SemanticTokensDeltaParams, SemanticTokensFullDeltaResult,
    SemanticTokensParams, SemanticTokensRangeParams, SemanticTokensRangeResult,
};

use super::Server;
use crate::line_index::LineIndex;
use crate::semantic_tokens;

impl Server<'_> {
    pub(super) fn semantic_tokens_full(
        &mut self,
        p: &SemanticTokensParams,
    ) -> Option<SemanticTokens> {
        let uri = &p.text_document.uri;
        let data = semantic_tokens::semantic_tokens(self.analysis(uri)?);
        Some(self.remember_tokens(uri, data))
    }

    pub(super) fn semantic_tokens_delta(
        &mut self,
        p: &SemanticTokensDeltaParams,
    ) -> Option<SemanticTokensFullDeltaResult> {
        let uri = &p.text_document.uri;
        let new = semantic_tokens::semantic_tokens(self.analysis(uri)?);
        let edits = match self.sent_tokens.get(uri) {
            Some((id, old)) if *id == p.previous_result_id => {
                Some(semantic_tokens::delta(old, &new))
            }
            _ => None,
        };
        let full = self.remember_tokens(uri, new);
        Some(match edits {
            Some(edits) => SemanticTokensFullDeltaResult::TokensDelta(SemanticTokensDelta {
                result_id: full.result_id,
                edits,
            }),
            None => SemanticTokensFullDeltaResult::Tokens(full),
        })
    }

    pub(super) fn semantic_tokens_range(
        &mut self,
        p: &SemanticTokensRangeParams,
    ) -> Option<SemanticTokensRangeResult> {
        let analysis = self.analysis(&p.text_document.uri)?;
        let index = LineIndex::new(analysis.text());
        let (lo, hi) = (index.offset(p.range.start), index.offset(p.range.end));
        let data = semantic_tokens::tokens_in(analysis, lo, hi);
        Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        }))
    }

    /// Keep `data` as the tokens last sent for `uri`, under a new result id.
    fn remember_tokens(
        &mut self,
        uri: &lsp_types::Url,
        data: Vec<lsp_types::SemanticToken>,
    ) -> SemanticTokens {
        self.next_result_id += 1;
        let id = self.next_result_id.to_string();
        self.sent_tokens
            .insert(uri.clone(), (id.clone(), data.clone()));
        SemanticTokens {
            result_id: Some(id),
            data,
        }
    }
}
