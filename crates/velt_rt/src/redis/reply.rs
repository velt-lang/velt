//! Reply conversion: a parsed reply tree becomes a `VeltRedisReply`, three parallel arrays in
//! pre-order (a node, then its children), which std/redis reads directly for typed commands
//! (`get` looks at node 0, `lrange` at nodes 1..) and rebuilds into a `RedisReply` tree for
//! `call()` and pipelines. Flat Copy arrays cross the C ABI without any per-node allocation on
//! the Velt side.

use super::error::RedisErr;
use super::resp::Value;
use crate::array::VeltArray;
use crate::bytes::VeltBytes;
use crate::str_array::VeltStrArray;

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

/// `{ u8[] tags; i64[] nums; string[] strs; }` (72 bytes): one entry per node, pre-order.
#[repr(C)]
pub struct VeltRedisReply {
    /// Node kinds ([`tag`]).
    pub tags: VeltBytes,
    /// Integer value (INT) or element count (ARRAY); 0 otherwise.
    pub nums: VeltArray<i64>,
    /// Text (STATUS, ERROR, STRING); `""` otherwise.
    pub strs: VeltStrArray,
}

/// The nodes of a reply before they become C arrays.
#[derive(Default, Debug, PartialEq)]
pub struct Flat {
    /// Node kinds.
    pub tags: Vec<u8>,
    /// Integers and array lengths.
    pub nums: Vec<i64>,
    /// Texts.
    pub strs: Vec<String>,
}

impl Flat {
    /// Append `v` and its children.
    pub fn push(&mut self, v: Value) {
        let (tag, num, text) = match v {
            Value::Nil => (tag::NIL, 0, String::new()),
            Value::Status(s) => (tag::STATUS, 0, s),
            Value::Error(s) => (tag::ERROR, 0, s),
            Value::Int(n) => (tag::INT, n, String::new()),
            Value::Bulk(b) => (tag::STRING, 0, into_text(b)),
            Value::Array(items) => {
                self.node(tag::ARRAY, items.len() as i64, String::new());
                for item in items {
                    self.push(item);
                }
                return;
            }
        };
        self.node(tag, num, text);
    }

    fn node(&mut self, tag: u8, num: i64, text: String) {
        self.tags.push(tag);
        self.nums.push(num);
        self.strs.push(text);
    }

    /// Hand the arrays over to Velt.
    pub fn into_velt(self) -> VeltRedisReply {
        VeltRedisReply {
            tags: VeltBytes::from_vec(self.tags),
            nums: VeltArray::from_vec(self.nums),
            strs: VeltStrArray::from_strings(self.strs),
        }
    }
}

fn into_text(b: Vec<u8>) -> String {
    String::from_utf8(b).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// A single command's reply: a top-level error reply fails the command.
pub fn single(v: Value) -> Result<Flat, RedisErr> {
    if let Value::Error(e) = v {
        return Err(RedisErr::server(e));
    }
    let mut f = Flat::default();
    f.push(v);
    Ok(f)
}

/// A pipeline's replies as one array node; failed commands are ERROR nodes, not failures.
pub fn pipeline(replies: Vec<Value>) -> Flat {
    let mut f = Flat::default();
    f.push(Value::Array(replies));
    f
}

/// A `MULTI` … `EXEC` block: `replies` = `+OK`, one `+QUEUED` (or error) per command, then
/// `EXEC`'s array. The result is `EXEC`'s array; an aborted transaction fails with its error,
/// naming the first command that was rejected.
pub fn transaction(mut replies: Vec<Value>) -> Result<Flat, RedisErr> {
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
        let f = single(v).unwrap();
        assert_eq!(f.tags, vec![5, 4, 5, 3, 0, 1, 4]);
        assert_eq!(f.nums, vec![4, 0, 2, 7, 0, 0, 0]);
        assert_eq!(f.strs, vec!["", "a", "", "", "", "OK", "\u{fffd}x"]);
    }

    #[test]
    fn errors_fail_single_commands_but_not_pipelines() {
        let e = single(Value::Error("WRONGTYPE nope".into())).unwrap_err();
        assert_eq!(e, RedisErr::server("WRONGTYPE nope".into()));
        let f = pipeline(vec![Value::Error("ERR x".into()), Value::Int(1)]);
        assert_eq!(f.tags, vec![tag::ARRAY, tag::ERROR, tag::INT]);
        assert_eq!(f.strs[1], "ERR x");
    }

    #[test]
    fn unwraps_transactions() {
        let ok = transaction(vec![
            Value::Status("OK".into()),
            Value::Status("QUEUED".into()),
            Value::Array(vec![Value::Int(3)]),
        ])
        .unwrap();
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

    #[test]
    fn hands_over_arrays() {
        let mut r = pipeline(vec![bulk("hi")]).into_velt();
        assert_eq!((r.tags.len, r.nums.len, r.strs.len), (2, 2, 2));
        unsafe {
            crate::bytes::velt_rt_bytes_drop(&mut r.tags);
            crate::str_array::velt_rt_str_array_drop(&mut r.strs);
            drop(Vec::from_raw_parts(r.nums.ptr, 2, r.nums.cap as usize));
        }
    }
}
