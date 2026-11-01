//! Reply conversion: a parsed reply tree becomes a `VeltRedisReply`, three parallel arrays in
//! pre-order (a node, then its children), which the package reads directly for typed commands
//! (`get` looks at node 0, `lrange` at nodes 1..) and rebuilds into a `RedisReply` tree for
//! `call()` and pipelines. Flat Copy arrays cross the boundary without any per-node allocation on
//! the Velt side.

use super::error::RedisErr;
use super::resp::Value;
use velt_native::{Array, Str};

/// Node kinds (`tags[i]`).
pub mod tag {
    /// A missing value (`$-1`, `*-1`).
    pub const NIL: u8 = 0;
    /// A status reply (`+OK`); text in `strs`.
    pub const STATUS: u8 = 1;
    /// An error reply nested in an array or pipeline; text in `strs`.
    pub const ERROR: u8 = 2;
    /// An integer; value in `nums`.
    pub const INT: u8 = 3;
    /// A bulk string; text in `strs` (invalid UTF-8 becomes U+FFFD).
    pub const STRING: u8 = 4;
    /// An array; element count in `nums`, elements follow.
    pub const ARRAY: u8 = 5;
}

/// The package's `RawReply` `{ tags: u8[]; nums: i64[]; strs: string[] }`: one entry per node,
/// pre-order.
#[derive(velt_native::Value)]
#[repr(C)]
pub struct RawReply {
    /// Node kinds ([`tag`]).
    pub tags: Array<u8>,
    /// Integer value (INT) or element count (ARRAY); 0 otherwise.
    pub nums: Array<i64>,
    /// Text (STATUS, ERROR, STRING); `""` otherwise.
    pub strs: Array<Str>,
}

/// Where a reply's nodes go, in pre-order: Velt's arrays ([`RawReply`]) or, in tests, [`Flat`].
pub trait Nodes {
    /// Room for `n` nodes.
    fn reserve(&mut self, n: usize);
    /// One node; `text` is UTF-8 (invalid bytes already replaced).
    fn node(&mut self, tag: u8, num: i64, text: &str);

    /// Append `v` and its children.
    fn push(&mut self, v: &Value) {
        match v {
            Value::Nil => self.node(tag::NIL, 0, ""),
            Value::Status(s) => self.node(tag::STATUS, 0, s),
            Value::Error(s) => self.node(tag::ERROR, 0, s),
            Value::Int(n) => self.node(tag::INT, *n, ""),
            Value::Bulk(b) => self.node(tag::STRING, 0, &String::from_utf8_lossy(b)),
            Value::Array(items) => {
                self.node(tag::ARRAY, items.len() as i64, "");
                for item in items {
                    self.push(item);
                }
            }
        }
    }

    /// `v` and its children, with the room reserved up front.
    fn of(v: &Value) -> Self
    where
        Self: Default,
    {
        let mut out = Self::default();
        out.reserve(count(v));
        out.push(v);
        out
    }
}

/// Nodes in `v` (itself and every descendant).
fn count(v: &Value) -> usize {
    match v {
        Value::Array(items) => 1 + items.iter().map(count).sum::<usize>(),
        _ => 1,
    }
}

impl Default for RawReply {
    fn default() -> RawReply {
        RawReply {
            tags: Array::new(),
            nums: Array::new(),
            strs: Array::new(),
        }
    }
}

/// Written straight into Velt's arrays (each text copied once into a Velt string; an empty one
/// is free).
impl Nodes for RawReply {
    fn reserve(&mut self, n: usize) {
        self.tags.reserve(n);
        self.nums.reserve(n);
        self.strs.reserve(n);
    }

    fn node(&mut self, tag: u8, num: i64, text: &str) {
        self.tags.push(tag);
        self.nums.push(num);
        self.strs.push(Str::new(text));
    }
}

/// A reply's nodes as Rust vectors (tests).
#[derive(Default, Debug, PartialEq)]
pub struct Flat {
    /// Node kinds.
    pub tags: Vec<u8>,
    /// Integers and array lengths.
    pub nums: Vec<i64>,
    /// Texts.
    pub strs: Vec<String>,
}

impl Nodes for Flat {
    fn reserve(&mut self, n: usize) {
        self.tags.reserve(n);
        self.nums.reserve(n);
        self.strs.reserve(n);
    }

    fn node(&mut self, tag: u8, num: i64, text: &str) {
        self.tags.push(tag);
        self.nums.push(num);
        self.strs.push(text.to_string());
    }
}

/// A single command's reply: a top-level error reply fails the command.
pub fn single(v: Value) -> Result<Value, RedisErr> {
    if let Value::Error(e) = v {
        return Err(RedisErr::server(e));
    }
    Ok(v)
}

/// A pipeline's replies as one array node; failed commands are ERROR nodes, not failures.
pub fn pipeline(replies: Vec<Value>) -> Value {
    Value::Array(replies)
}

/// A `MULTI` … `EXEC` block: `replies` = `+OK`, one `+QUEUED` (or error) per command, then
/// `EXEC`'s array. The result is `EXEC`'s array; an aborted transaction fails with its error,
/// naming the first command that was rejected.
pub fn transaction(mut replies: Vec<Value>) -> Result<Value, RedisErr> {
    let exec = replies.pop().unwrap_or(Value::Nil);
    let rejected = replies.iter().find_map(|v| match v {
        Value::Error(e) => Some(e.clone()),
        _ => None,
    });
    match (exec, rejected) {
        (Value::Error(e), Some(first)) => Err(RedisErr::server(format!("{e} ({first})"))),
        (Value::Error(e), None) => Err(RedisErr::server(e)),
        (Value::Nil, _) => Err(RedisErr::server(
            "EXECABORT Transaction aborted (a watched key changed)".to_string(),
        )),
        (exec, _) => Ok(pipeline(match exec {
            Value::Array(items) => items,
            other => vec![other],
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::Bulk(s.as_bytes().to_vec())
    }

    #[test]
    fn flattens_in_pre_order() {
        let v = Value::Array(vec![
            bulk("a"),
            Value::Array(vec![Value::Int(7), Value::Nil]),
            Value::Status("OK".into()),
            Value::Bulk(vec![0xff, b'x']),
        ]);
        let f = Flat::of(&single(v).unwrap());
        assert_eq!(f.tags, vec![5, 4, 5, 3, 0, 1, 4]);
        assert_eq!(f.nums, vec![4, 0, 2, 7, 0, 0, 0]);
        assert_eq!(f.strs, vec!["", "a", "", "", "", "OK", "\u{fffd}x"]);
    }

    #[test]
    fn errors_fail_single_commands_but_not_pipelines() {
        let e = single(Value::Error("WRONGTYPE nope".into())).unwrap_err();
        assert_eq!(e, RedisErr::server("WRONGTYPE nope".into()));
        let f = Flat::of(&pipeline(vec![Value::Error("ERR x".into()), Value::Int(1)]));
        assert_eq!(f.tags, vec![tag::ARRAY, tag::ERROR, tag::INT]);
        assert_eq!(f.strs[1], "ERR x");
    }

    #[test]
    fn unwraps_transactions() {
        let ok = Flat::of(
            &transaction(vec![
                Value::Status("OK".into()),
                Value::Status("QUEUED".into()),
                Value::Array(vec![Value::Int(3)]),
            ])
            .unwrap(),
        );
        assert_eq!((ok.tags, ok.nums), (vec![5, 3], vec![1, 3]));
        let aborted = transaction(vec![
            Value::Status("OK".into()),
            Value::Error("ERR unknown command 'NOPE'".into()),
            Value::Error("EXECABORT Transaction discarded".into()),
        ])
        .unwrap_err();
        assert!(aborted.message.starts_with("EXECABORT"));
        assert!(aborted.message.contains("NOPE"));
    }
}
