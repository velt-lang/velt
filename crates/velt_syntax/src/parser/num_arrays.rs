//! A large array literal of number literals as one packed node ([`NumArray`], #2421): a scan
//! of the tokens decides, so any other array literal parses as before.

use super::Parser;
use crate::ast::{ExprKind, NodeId, NumArray, NumElem};
use crate::lexer::{Payload, Tok};

impl<'a> Parser<'a> {
    /// At `[`: the array literal as a [`NumArray`] when it holds at least
    /// [`NumArray::MIN_LEN`] elements and each is a number literal without a suffix (an integer
    /// that fits a `u64`), maybe negated, up to the `]` (a trailing comma allowed); the tokens up
    /// to the `]` consumed. `None` (nothing consumed) for any other array literal.
    pub(super) fn num_array(&mut self) -> Option<ExprKind> {
        if !self.pack_numbers {
            return None;
        }
        let n = self.num_array_len()?;
        let first_id = NodeId(self.next_id);
        self.bump(); // [
        let mut elems = Vec::with_capacity(n);
        for _ in 0..n {
            let lo = self.cur_lo();
            if self.at(Tok::Minus) {
                self.bump();
            }
            let t = self.tok(self.pos);
            let (bits, float) = self.num_literal(t.kind).expect("ICE: scanned as a number");
            self.bump();
            let neg = lo != t.lo;
            self.next_id += 1 + u32::from(neg);
            elems.push(NumElem {
                bits,
                lo,
                lit_lo: t.lo,
                hi: t.hi,
                float,
            });
            self.eat(Tok::Comma);
        }
        self.bump(); // ]
        Some(ExprKind::NumArray(Box::new(NumArray { first_id, elems })))
    }

    /// The number of elements, if the array literal at the cursor is one [`Self::num_array`]
    /// packs.
    fn num_array_len(&mut self) -> Option<usize> {
        let mut i = self.pos + 1;
        let mut n = 0;
        loop {
            if self.tok(i).kind == Tok::Minus {
                i += 1;
            }
            let t = self.tok(i).kind;
            self.num_literal(t)?;
            n += 1;
            i += 1;
            match self.tok(i).kind {
                Tok::Comma if self.tok(i + 1).kind == Tok::RBracket => break,
                Tok::Comma => i += 1,
                Tok::RBracket => break,
                _ => return None,
            }
        }
        (n >= NumArray::MIN_LEN).then_some(n)
    }

    /// The value of number literal token `t` (an `f64`'s bits for a float, with `true`), if it
    /// is one without a suffix whose value fits a `u64`.
    fn num_literal(&self, t: Tok) -> Option<(u64, bool)> {
        match t {
            Tok::Int(i) => match self.payload(i)? {
                Payload::Int {
                    value,
                    suffix: None,
                } => Some((u64::try_from(*value).ok()?, false)),
                _ => None,
            },
            Tok::Float(i) => match self.payload(i)? {
                Payload::Float {
                    value,
                    suffix: None,
                } => Some((value.to_bits(), true)),
                _ => None,
            },
            _ => None,
        }
    }
}
