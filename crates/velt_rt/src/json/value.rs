//! `json.Value`: an immutable tree of `Arc`'d nodes built by `JSON.parseValue`.
//!
//! Handles given to generated code are `Arc::into_raw` pointers, so a handle to a child stays
//! valid after the root handle is freed. Parsing, stringifying and dropping are iterative, so
//! arbitrarily deep documents cannot overflow the stack.

use super::scan::{number_f64, Scanner, StrTok, SyntaxError};
use super::walk::{walk, Scalar, Sink};
use crate::fmt;
use std::collections::HashMap;
use std::sync::Arc;

/// Objects with more keys than this get a hash index for `get` and duplicate-key detection.
const INDEX_THRESHOLD: usize = 16;

/// A parsed JSON value.
#[derive(Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(Box<str>),
    Array(Vec<Arc<Value>>),
    Object(Object),
}

/// Object members in document order (first-occurrence position, last value wins — like
/// `JSON.parse`).
#[derive(Debug, Default)]
pub struct Object {
    pub entries: Vec<(Box<str>, Arc<Value>)>,
    index: Option<HashMap<Box<str>, usize>>,
}

impl Object {
    /// Position of `key`.
    pub fn find(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(index) => index.get(key).copied(),
            None => self.entries.iter().position(|(k, _)| &**k == key),
        }
    }

    /// Insert, replacing the value of an existing key in place.
    fn insert(&mut self, key: Box<str>, value: Arc<Value>) {
        if let Some(i) = self.find(&key) {
            self.entries[i].1 = value;
            return;
        }
        if let Some(index) = &mut self.index {
            index.insert(key.clone(), self.entries.len());
        } else if self.entries.len() == INDEX_THRESHOLD {
            let mut index: HashMap<Box<str>, usize> = self
                .entries
                .iter()
                .enumerate()
                .map(|(i, (k, _))| (k.clone(), i))
                .collect();
            index.insert(key.clone(), self.entries.len());
            self.index = Some(index);
        }
        self.entries.push((key, value));
    }
}

impl Value {
    /// Move out all children (leaves `self` childless).
    fn take_children(&mut self) -> Vec<Arc<Value>> {
        match self {
            Value::Array(items) => std::mem::take(items),
            Value::Object(obj) => {
                obj.index = None;
                std::mem::take(&mut obj.entries)
                    .into_iter()
                    .map(|(_, v)| v)
                    .collect()
            }
            _ => Vec::new(),
        }
    }
}

impl Drop for Value {
    /// Iterative teardown: nodes whose last reference is dropped here are emptied onto a work
    /// list instead of recursing.
    fn drop(&mut self) {
        let mut pending = match self {
            Value::Array(items) if !items.is_empty() => std::mem::take(items),
            Value::Object(obj) if !obj.entries.is_empty() => self.take_children(),
            _ => return,
        };
        while let Some(child) = pending.pop() {
            if let Ok(mut node) = Arc::try_unwrap(child) {
                pending.extend(node.take_children());
            }
        }
    }
}

fn owned_text(src: &[u8], tok: StrTok) -> Box<str> {
    let bytes = match tok {
        StrTok::Borrowed(start, end) => src[start..end].to_vec(),
        StrTok::Owned(v) => v,
    };
    // SAFETY: the source is UTF-8 (VeltStr invariant) and escapes decode to UTF-8.
    unsafe { String::from_utf8_unchecked(bytes) }.into_boxed_str()
}

/// A container being built: its members so far and, for objects, the key awaiting its value.
enum Frame {
    Array(Vec<Arc<Value>>),
    Object(Object, Option<Box<str>>),
}

/// Tree-building sink.
#[derive(Default)]
struct Builder {
    stack: Vec<Frame>,
    root: Option<Arc<Value>>,
}

impl Builder {
    /// Where the builder is: `$` and a segment per open container (`.key` while a member's
    /// value is being read, `[i]` for the next element), like the typed decoders' paths.
    fn path(&self) -> String {
        let mut p = String::from("$");
        for f in &self.stack {
            match f {
                Frame::Array(items) => p.push_str(&format!("[{}]", items.len())),
                Frame::Object(_, Some(key)) => {
                    p.push('.');
                    p.push_str(key);
                }
                Frame::Object(_, None) => {}
            }
        }
        p
    }

    fn add(&mut self, value: Value) {
        let value = Arc::new(value);
        match self.stack.last_mut() {
            Some(Frame::Array(items)) => items.push(value),
            Some(Frame::Object(obj, key)) => {
                let key = key.take().expect("ICE: object value without key");
                obj.insert(key, value);
            }
            None => self.root = Some(value),
        }
    }
}

impl Sink for Builder {
    const DECODE: bool = true;
    fn begin_array(&mut self) {
        self.stack.push(Frame::Array(Vec::new()));
    }
    fn begin_object(&mut self) {
        self.stack.push(Frame::Object(Object::default(), None));
    }
    fn key(&mut self, src: &[u8], key: StrTok) {
        if let Some(Frame::Object(_, slot)) = self.stack.last_mut() {
            *slot = Some(owned_text(src, key));
        }
    }
    fn end(&mut self) {
        let value = match self.stack.pop().expect("ICE: unbalanced JSON walk") {
            Frame::Array(items) => Value::Array(items),
            Frame::Object(obj, _) => Value::Object(obj),
        };
        self.add(value);
    }
    fn scalar(&mut self, src: &[u8], value: Scalar) {
        self.add(match value {
            Scalar::Null => Value::Null,
            Scalar::Bool(b) => Value::Bool(b),
            Scalar::Number(tok) => Value::Number(number_f64(src, tok)),
            Scalar::Str(tok) => Value::String(owned_text(src, tok)),
        });
    }
}

/// Build the tree of the one value starting at the scanner's position.
pub fn read(sc: &mut Scanner) -> Result<Arc<Value>, SyntaxError> {
    let mut builder = Builder::default();
    walk(sc, &mut builder)?;
    Ok(builder.root.expect("ICE: JSON walk produced no value"))
}

/// Parse a whole document (one value, surrounded only by whitespace). An error comes with the
/// path of the value it is in.
pub fn parse(src: &[u8]) -> Result<Arc<Value>, (SyntaxError, String)> {
    let mut sc = Scanner::new(src);
    let mut builder = Builder::default();
    if let Err(e) = walk(&mut sc, &mut builder) {
        return Err((e, builder.path()));
    }
    if sc.peek_non_ws().is_some() {
        return Err((sc.error("unexpected trailing characters"), "$".into()));
    }
    Ok(builder.root.expect("ICE: JSON walk produced no value"))
}

/// A value being written with the index of its next child.
enum Open<'a> {
    Array(&'a [Arc<Value>], usize),
    Object(&'a [(Box<str>, Arc<Value>)], usize),
}

/// Append `JSON.stringify(v)`: key order preserved, numbers JS-formatted (non-finite → `null`).
pub fn stringify_into(out: &mut Vec<u8>, v: &Value) {
    let mut stack: Vec<Open> = Vec::new();
    let mut next = Some(v);
    loop {
        if let Some(v) = next.take() {
            match v {
                Value::Null => out.extend_from_slice(b"null"),
                Value::Bool(b) => fmt::push_bool(out, *b as u8),
                Value::Number(n) if n.is_finite() => fmt::push_f64(out, *n),
                Value::Number(_) => out.extend_from_slice(b"null"),
                Value::String(s) => super::escape::push_json_string(out, s.as_bytes()),
                Value::Array(items) => {
                    out.push(b'[');
                    stack.push(Open::Array(items, 0));
                }
                Value::Object(obj) => {
                    out.push(b'{');
                    stack.push(Open::Object(&obj.entries, 0));
                }
            }
        }
        // Pick the next child of the innermost open container, or close it.
        let Some(top) = stack.last_mut() else {
            return;
        };
        match top {
            Open::Array(items, i) if *i < items.len() => {
                if *i > 0 {
                    out.push(b',');
                }
                next = Some(&items[*i]);
                *i += 1;
            }
            Open::Object(entries, i) if *i < entries.len() => {
                if *i > 0 {
                    out.push(b',');
                }
                let (key, value) = &entries[*i];
                super::escape::push_json_string(out, key.as_bytes());
                out.push(b':');
                next = Some(value);
                *i += 1;
            }
            Open::Array(..) => {
                out.push(b']');
                stack.pop();
            }
            Open::Object(..) => {
                out.push(b'}');
                stack.pop();
            }
        }
    }
}
