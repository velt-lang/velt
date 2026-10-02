//! Iterative (no recursion) walk over one JSON value, reporting structure to a [`Sink`], with
//! an optional limit on nesting. Used by `skip_value` (a no-op sink), by union lookahead (a
//! sink remembering where containers end) and by the `json.Value` parser (a tree builder).

use super::scan::{NumTok, Scanner, StrTok, SyntaxError, TOO_DEEP};
use std::collections::HashMap;

/// A scalar JSON value.
#[derive(Debug)]
pub enum Scalar {
    Null,
    Bool(bool),
    Number(NumTok),
    Str(StrTok),
}

/// Receives the structure of a value in document order.
pub trait Sink {
    /// Whether strings with escapes must be decoded (a skipping sink only validates them).
    const DECODE: bool;
    /// An array opens; `at` is the byte offset of its `[`.
    fn begin_array(&mut self, at: usize);
    /// An object opens; `at` is the byte offset of its `{`.
    fn begin_object(&mut self, at: usize);
    /// An object key; the next event is its value.
    fn key(&mut self, src: &[u8], key: StrTok);
    /// Closes the innermost open array/object; `at` is the byte offset just past its bracket.
    fn end(&mut self, at: usize);
    fn scalar(&mut self, src: &[u8], value: Scalar);
}

/// Validates without building anything.
pub struct SkipSink;

impl Sink for SkipSink {
    const DECODE: bool = false;
    fn begin_array(&mut self, _: usize) {}
    fn begin_object(&mut self, _: usize) {}
    fn key(&mut self, _: &[u8], _: StrTok) {}
    fn end(&mut self, _: usize) {}
    fn scalar(&mut self, _: &[u8], _: Scalar) {}
}

/// Validates like [`SkipSink`] and remembers where each array/object ends (by the offset of
/// its opening bracket), so a union decoder looking ahead can jump over it the next time.
pub struct MemoSink<'a> {
    pub ends: &'a mut HashMap<usize, usize>,
    pub open: Vec<usize>,
}

impl Sink for MemoSink<'_> {
    const DECODE: bool = false;
    fn begin_array(&mut self, at: usize) {
        self.open.push(at);
    }
    fn begin_object(&mut self, at: usize) {
        self.open.push(at);
    }
    fn key(&mut self, _: &[u8], _: StrTok) {}
    fn end(&mut self, at: usize) {
        let start = self.open.pop().expect("ICE: unbalanced JSON walk");
        self.ends.insert(start, at);
    }
    fn scalar(&mut self, _: &[u8], _: Scalar) {}
}

/// Stack of open containers (`true` = object): one word inline, spilling past depth 64.
struct ContainerStack {
    inline: u64,
    spill: Vec<bool>,
    depth: usize,
}

impl ContainerStack {
    fn push(&mut self, object: bool) {
        if self.depth < 64 {
            self.inline = (self.inline & !(1 << self.depth)) | ((object as u64) << self.depth);
        } else {
            self.spill.push(object);
        }
        self.depth += 1;
    }

    fn top(&self) -> Option<bool> {
        let d = self.depth.checked_sub(1)?;
        Some(if d < 64 {
            self.inline >> d & 1 == 1
        } else {
            self.spill[d - 64]
        })
    }

    fn pop(&mut self) {
        self.depth -= 1;
        if self.depth >= 64 {
            self.spill.pop();
        }
    }
}

/// Lex one scalar at the current position.
pub fn scalar(sc: &mut Scanner, decode: bool) -> Result<Scalar, SyntaxError> {
    match sc.peek_non_ws() {
        Some(b'"') => Ok(Scalar::Str(sc.string(decode)?)),
        Some(b't') => sc.literal(b"true").map(|_| Scalar::Bool(true)),
        Some(b'f') => sc.literal(b"false").map(|_| Scalar::Bool(false)),
        Some(b'n') => sc.literal(b"null").map(|_| Scalar::Null),
        Some(b'-' | b'0'..=b'9') => Ok(Scalar::Number(sc.number()?)),
        _ => Err(sc.unexpected()),
    }
}

/// `"key" :` inside an object.
fn key_colon<S: Sink>(sc: &mut Scanner, sink: &mut S) -> Result<(), SyntaxError> {
    if sc.peek_non_ws() != Some(b'"') {
        return Err(sc.error("expected string key"));
    }
    let key = sc.string(S::DECODE)?;
    sink.key(sc.src, key);
    if sc.peek_non_ws() != Some(b':') {
        return Err(sc.error("expected ':'"));
    }
    sc.pos += 1;
    Ok(())
}

/// Walk exactly one value starting at the current position (leading whitespace allowed),
/// failing with [`TOO_DEEP`] at an array/object nested more than `limit` deep.
pub fn walk_limited<S: Sink>(
    sc: &mut Scanner,
    sink: &mut S,
    limit: usize,
) -> Result<(), SyntaxError> {
    let mut stack = ContainerStack {
        inline: 0,
        spill: Vec::new(),
        depth: 0,
    };
    loop {
        // At a value position.
        match sc.peek_non_ws() {
            Some(open @ (b'{' | b'[')) => {
                if stack.depth >= limit {
                    return Err(sc.error(TOO_DEEP));
                }
                let at = sc.pos;
                sc.pos += 1;
                let object = open == b'{';
                if object {
                    sink.begin_object(at);
                } else {
                    sink.begin_array(at);
                }
                if sc.peek_non_ws() == Some(if object { b'}' } else { b']' }) {
                    sc.pos += 1;
                    sink.end(sc.pos);
                } else {
                    stack.push(object);
                    if object {
                        key_colon(sc, sink)?;
                    }
                    continue;
                }
            }
            _ => {
                let value = scalar(sc, S::DECODE)?;
                sink.scalar(sc.src, value);
            }
        }
        // A value just completed: close containers until one expects another element.
        loop {
            let Some(object) = stack.top() else {
                return Ok(());
            };
            match sc.peek_non_ws() {
                Some(b',') => {
                    sc.pos += 1;
                    if object {
                        key_colon(sc, sink)?;
                    }
                    break;
                }
                Some(b'}') if object => {
                    sc.pos += 1;
                    stack.pop();
                    sink.end(sc.pos);
                }
                Some(b']') if !object => {
                    sc.pos += 1;
                    stack.pop();
                    sink.end(sc.pos);
                }
                _ if object => return Err(sc.error("expected ',' or '}'")),
                _ => return Err(sc.error("expected ',' or ']'")),
            }
        }
    }
}
