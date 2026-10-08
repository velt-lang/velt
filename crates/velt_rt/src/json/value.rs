//! `json.Value`: a tree of `Arc`'d nodes built by `JSON.parseValue` or edited through
//! `value_edit` (copy-on-write: a node shared with another handle is copied before a change).
//!
//! Handles given to generated code are `Arc::into_raw` pointers, so a handle to a child stays
//! valid after the root handle is freed. Parsing, stringifying and dropping are iterative, so
//! arbitrarily deep documents cannot overflow the stack.

use super::object::count_work;
pub use super::object::Object;
use super::scan::{number_f64, Scanner, StrTok, SyntaxError};
use super::walk::{walk_limited, Scalar, Sink};
use std::sync::Arc;

/// The text of a string value or an object key: canonical WTF-8, as a Velt string's bytes (it
/// may hold lone surrogates, #377).
pub type Text = Box<[u8]>;

/// A parsed JSON value.
#[derive(Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(Text),
    Array(Vec<Arc<Value>>),
    Object(Object),
}

impl Value {
    /// A copy of this node sharing its children (O(number of children), counted as work).
    pub fn shallow_clone(&self) -> Value {
        match self {
            Value::Array(items) => count_work(items.len()),
            Value::Object(obj) => count_work(obj.len()),
            _ => {}
        }
        match self {
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(*b),
            Value::Number(n) => Value::Number(*n),
            Value::String(s) => Value::String(s.clone()),
            Value::Array(items) => Value::Array(items.clone()),
            Value::Object(obj) => Value::Object(obj.clone()),
        }
    }

    /// Move out all children (leaves `self` childless).
    fn take_children(&mut self) -> Vec<Arc<Value>> {
        match self {
            Value::Array(items) => std::mem::take(items),
            Value::Object(obj) => obj.take_values(),
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
            Value::Object(obj) if !obj.is_empty() => self.take_children(),
            _ => return,
        };
        while let Some(child) = pending.pop() {
            if let Ok(mut node) = Arc::try_unwrap(child) {
                pending.extend(node.take_children());
            }
        }
    }
}

/// A string token's text: a range of the source (a Velt string, canonical WTF-8) or decoded
/// bytes (escapes decode to code units, joined where a high half meets a low one), so canonical
/// WTF-8.
fn owned_text(src: &[u8], tok: StrTok) -> Text {
    match tok {
        StrTok::Borrowed(start, end) => src[start..end].into(),
        StrTok::Owned(v) => v.into_boxed_slice(),
    }
}

/// A container being built: its members so far and, for objects, the key awaiting its value.
enum Frame {
    Array(Vec<Arc<Value>>),
    Object(Object, Option<Text>),
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
                    p.push_str(&crate::str::wtf8::to_utf8_lossy(key));
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
    fn begin_array(&mut self, _: usize) {
        self.stack.push(Frame::Array(Vec::new()));
    }
    fn begin_object(&mut self, _: usize) {
        self.stack.push(Frame::Object(Object::default(), None));
    }
    fn key(&mut self, src: &[u8], key: StrTok) {
        if let Some(Frame::Object(_, slot)) = self.stack.last_mut() {
            *slot = Some(owned_text(src, key));
        }
    }
    fn end(&mut self, _: usize) {
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

/// Build the tree of the one value starting at the scanner's position, nested at most
/// `limit` deep.
pub fn read_limited(sc: &mut Scanner, limit: usize) -> Result<Arc<Value>, SyntaxError> {
    let mut builder = Builder::default();
    walk_limited(sc, &mut builder, limit)?;
    Ok(builder.root.expect("ICE: JSON walk produced no value"))
}

/// Parse a whole document (one value, surrounded only by whitespace), nested at most `limit`
/// deep. An error comes with the path of the value it is in.
pub fn parse(src: &[u8], limit: usize) -> Result<Arc<Value>, (SyntaxError, String)> {
    let mut sc = Scanner::new(src);
    let mut builder = Builder::default();
    if let Err(e) = walk_limited(&mut sc, &mut builder, limit) {
        return Err((e, builder.path()));
    }
    if sc.peek_non_ws().is_some() {
        return Err((sc.error("unexpected trailing characters"), "$".into()));
    }
    Ok(builder.root.expect("ICE: JSON walk produced no value"))
}
