//! Matching parentheses, found on demand and cached by token index. The parser asks "what follows
//! the `)` of this `(`?" to tell function types and arrow functions from parenthesized
//! expressions without speculating, which would re-parse nested parentheses exponentially often.
//!
//! The cache must survive the parser re-lexing a `<` as JSX (the tokens from there on change) at
//! a cost proportional to what changed, not to the file so far: it is indexed by token, so
//! forgetting everything from the re-lex point on is a truncation, plus the pairs that straddle
//! the point, which are found through their `)`.

use super::Parser;
use crate::lexer::Tok;

/// `partner` entry of a token not scanned yet (or not a parenthesis).
const UNKNOWN: u32 = u32::MAX;
/// `partner` entry of a `(` whose scan reached the end of the file without a matching `)`.
const UNMATCHED: u32 = u32::MAX - 1;

/// Known `(`/`)` pairs and unmatched `(`s, by token index.
#[derive(Default)]
pub(super) struct ParenMatches {
    /// By token index: a `(`'s matching `)`, a `)`'s matching `(`, or a marker. An entry
    /// smaller than its own index is therefore a `)`.
    partner: Vec<u32>,
    /// The `(`s recorded as unmatched. Their scan ran to the end of the file, past any re-lex
    /// point, so every re-lex invalidates all of them.
    unmatched: Vec<usize>,
    /// The open `(`s of the scan in progress, kept to reuse its allocation.
    stack: Vec<usize>,
}

impl ParenMatches {
    /// `None`: unknown; `Some(None)`: unmatched; `Some(Some(close))`: matched.
    fn get(&self, open: usize) -> Option<Option<usize>> {
        match self.partner.get(open).copied().unwrap_or(UNKNOWN) {
            UNKNOWN => None,
            UNMATCHED => Some(None),
            close => Some(Some(close as usize)),
        }
    }

    fn set(&mut self, i: usize, entry: u32) {
        if self.partner.len() <= i {
            self.partner.resize(i + 1, UNKNOWN);
        }
        self.partner[i] = entry;
    }

    fn insert_pair(&mut self, open: usize, close: usize) {
        self.set(open, close as u32);
        self.set(close, open as u32);
    }

    fn insert_unmatched(&mut self, open: usize) {
        self.set(open, UNMATCHED);
        self.unmatched.push(open);
    }

    /// Drops every entry that looked at token `i` or later: pairs opened or closed there, and
    /// all unmatched `(`s. Linear in the number of entries dropped, whose tokens the lexer
    /// drops too.
    pub(super) fn forget_from(&mut self, i: usize) {
        let split = i.min(self.partner.len());
        let (before, after) = self.partner.split_at_mut(split);
        crate::work::add(after.len() + self.unmatched.len());
        for &entry in after.iter() {
            // A `)` at or after `i` whose `(` comes before it.
            if let Some(open) = before.get_mut(entry as usize) {
                *open = UNKNOWN;
            }
        }
        self.partner.truncate(i);
        for open in self.unmatched.drain(..) {
            if let Some(entry) = self.partner.get_mut(open) {
                *entry = UNKNOWN;
            }
        }
    }
}

impl Parser<'_> {
    /// The token after the `)` matching the `(` at `pos + off`, if that `(` is matched.
    pub(super) fn after_matching_paren(&mut self, off: usize) -> Option<Tok> {
        let open = self.pos + off;
        if self.tok(open).kind != Tok::LParen {
            return None;
        }
        let close = self.matching_paren(open)?;
        Some(self.tok(close + 1).kind)
    }

    /// Index of the `)` matching the `(` at token `open`: one forward scan with a stack of open
    /// parentheses that records every pair it closes, so each token is scanned about once.
    fn matching_paren(&mut self, open: usize) -> Option<usize> {
        if let Some(known) = self.paren_matches.get(open) {
            return known;
        }
        let mut stack = std::mem::take(&mut self.paren_matches.stack);
        stack.clear();
        stack.push(open);
        let mut i = open + 1;
        loop {
            crate::work::add(1);
            match self.tok(i).kind {
                Tok::LParen => match self.paren_matches.get(i) {
                    Some(Some(close)) => {
                        i = close + 1;
                        continue;
                    }
                    Some(None) => break,
                    None => stack.push(i),
                },
                Tok::RParen => {
                    let o = stack.pop().expect("ICE: paren stack never empties early");
                    self.paren_matches.insert_pair(o, i);
                    if stack.is_empty() {
                        self.paren_matches.stack = stack;
                        return Some(i);
                    }
                }
                Tok::Eof => break,
                _ => {}
            }
            i += 1;
        }
        for &o in &stack {
            self.paren_matches.insert_unmatched(o);
        }
        self.paren_matches.stack = stack;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::ParenMatches;

    #[test]
    fn forgetting_keeps_only_pairs_before_the_point() {
        let mut m = ParenMatches::default();
        m.insert_pair(0, 2); // before the point
        m.insert_pair(3, 9); // straddles it
        m.insert_pair(6, 8); // after it
        m.insert_unmatched(1);
        m.forget_from(5);
        assert_eq!(m.get(0), Some(Some(2)));
        assert_eq!(m.get(3), None);
        assert_eq!(m.get(6), None);
        assert_eq!(m.get(1), None);
        assert!(m.partner.len() <= 5);
    }
}
