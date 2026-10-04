//! The two texts of a `json.Value` tree: `JSON.stringify` (`stringify_into`) and what
//! `console.log` prints (`inspect_into`, node's `util.inspect` of the parsed object, within its
//! `depth` and `maxArrayLength` limits). One iterative walk serves both, so arbitrarily deep
//! values cannot overflow the stack.

use super::escape::push_json_string;
use super::object::Entries;
use super::value::Value;
use crate::fmt;
use crate::inspect::{push_inspect_key, push_inspect_string, push_more_items};
use crate::inspect::{DEPTH, MAX_ARRAY_LENGTH};
use std::sync::Arc;

/// How a text form writes the parts of a tree.
trait Style {
    /// A string value inside a container (or at the top level, for JSON).
    fn string(out: &mut Vec<u8>, s: &[u8]);
    /// The opening of a container with members (`[`, `{` / `[ `, `{ `).
    const OPEN: [&'static [u8]; 2];
    /// Between two members.
    const SEP: &'static [u8];
    /// The closing of a container with members.
    const CLOSE: [&'static [u8]; 2];
    /// An object key with what separates it from its value.
    fn key(out: &mut Vec<u8>, k: &[u8]);
    /// Node's `depth` and `maxArrayLength` apply.
    const LIMITS: bool;
}

/// `JSON.stringify`: no whitespace, numbers JS-formatted (non-finite → `null`).
struct Json;

impl Style for Json {
    fn string(out: &mut Vec<u8>, s: &[u8]) {
        push_json_string(out, s);
    }
    const OPEN: [&'static [u8]; 2] = [b"[", b"{"];
    const SEP: &'static [u8] = b",";
    const CLOSE: [&'static [u8]; 2] = [b"]", b"}"];
    fn key(out: &mut Vec<u8>, k: &[u8]) {
        push_json_string(out, k);
        out.push(b':');
    }
    const LIMITS: bool = false;
}

/// `console.log`: `{ a: 1, b: [ 2, 'x' ] }`, `[]`, `{}`, like Velt's other values (one line).
struct Inspect;

impl Style for Inspect {
    fn string(out: &mut Vec<u8>, s: &[u8]) {
        push_inspect_string(out, s);
    }
    const OPEN: [&'static [u8]; 2] = [b"[ ", b"{ "];
    const SEP: &'static [u8] = b", ";
    const CLOSE: [&'static [u8]; 2] = [b" ]", b" }"];
    fn key(out: &mut Vec<u8>, k: &[u8]) {
        push_inspect_key(out, k);
        out.extend_from_slice(b": ");
    }
    const LIMITS: bool = true;
}

/// Append `JSON.stringify(v)`: key order preserved, numbers JS-formatted (non-finite → `null`).
pub fn stringify_into(out: &mut Vec<u8>, v: &Value) {
    write_tree::<Json>(out, v, 0);
}

/// Append what `console.log` prints for `v`: a string raw at the top level (`top`), quoted
/// inside containers and when not `top` (a member of another printed value). `depth` is node's
/// depth of `v` (0 for a `console.log` argument): containers deeper than 2 print as `[Array]` /
/// `[Object]`.
pub fn inspect_into(out: &mut Vec<u8>, v: &Value, top: bool, depth: u32) {
    match v {
        Value::String(s) if top => out.extend_from_slice(s),
        _ => write_tree::<Inspect>(out, v, depth),
    }
}

/// A container being written: the members still to write (for an array, also how many more
/// may be shown), and whether one was written.
enum Open<'a> {
    Array(std::slice::Iter<'a, Arc<Value>>, usize),
    Object(Entries<'a>),
}

/// Write `v` (at node's depth `depth`) in style `S`, iteratively.
fn write_tree<S: Style>(out: &mut Vec<u8>, v: &Value, depth: u32) {
    let mut stack: Vec<(Open, bool)> = Vec::new();
    let mut next = Some(v);
    let room = if S::LIMITS {
        MAX_ARRAY_LENGTH
    } else {
        usize::MAX
    };
    loop {
        if let Some(v) = next.take() {
            let deep = S::LIMITS && depth as usize + stack.len() > DEPTH as usize;
            match v {
                Value::Null => out.extend_from_slice(b"null"),
                Value::Bool(b) => fmt::push_bool(out, *b as u8),
                Value::Number(n) if n.is_finite() => fmt::push_f64(out, *n),
                Value::Number(_) => out.extend_from_slice(b"null"),
                Value::String(s) => S::string(out, s),
                Value::Array(items) if items.is_empty() => out.extend_from_slice(b"[]"),
                Value::Object(obj) if obj.is_empty() => out.extend_from_slice(b"{}"),
                Value::Array(_) if deep => out.extend_from_slice(b"[Array]"),
                Value::Object(_) if deep => out.extend_from_slice(b"[Object]"),
                Value::Array(items) => {
                    out.extend_from_slice(S::OPEN[0]);
                    stack.push((Open::Array(items.iter(), room), false));
                }
                Value::Object(obj) => {
                    out.extend_from_slice(S::OPEN[1]);
                    stack.push((Open::Object(obj.iter()), false));
                }
            }
        }
        // Pick the next member of the innermost open container, or close it.
        let Some((top, started)) = stack.last_mut() else {
            return;
        };
        let member = match top {
            Open::Array(items, 0) => {
                if items.len() > 0 {
                    push_more_items(out, items.len() as u64);
                }
                None
            }
            Open::Array(items, room) => {
                *room -= 1;
                items.next().map(|v| (None, v))
            }
            Open::Object(entries) => entries.next().map(|(k, v)| (Some(k), v)),
        };
        match member {
            Some((key, value)) => {
                if std::mem::replace(started, true) {
                    out.extend_from_slice(S::SEP);
                }
                if let Some(k) = key {
                    S::key(out, k);
                }
                next = Some(value);
            }
            None => {
                let close = S::CLOSE[matches!(top, Open::Object(_)) as usize];
                out.extend_from_slice(close);
                stack.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{inspect_into, stringify_into};
    use crate::json::value::parse;

    fn both(src: &str, top: bool) -> (String, String) {
        let v = parse(src.as_bytes(), usize::MAX).unwrap();
        let (mut json, mut shown) = (Vec::new(), Vec::new());
        stringify_into(&mut json, &v);
        inspect_into(&mut shown, &v, top, 0);
        (
            String::from_utf8(json).unwrap(),
            String::from_utf8(shown).unwrap(),
        )
    }

    #[test]
    fn inspect_prints_like_node() {
        let src = r#"{"a":1,"b":[2,"x"],"c":null,"d":{},"e":[],"it's":"it's","$":-1.5}"#;
        let (json, shown) = both(src, true);
        assert_eq!(json, src);
        assert_eq!(
            shown,
            r#"{ a: 1, b: [ 2, 'x' ], c: null, d: {}, e: [], "it's": "it's", '$': -1.5 }"#
        );
        assert_eq!(both(r#""s""#, true).1, "s");
        assert_eq!(both(r#""s""#, false).1, "'s'");
    }

    #[test]
    fn deep_values_print_without_recursion() {
        let depth = 100_000;
        let src = "[".repeat(depth) + &"]".repeat(depth);
        let (json, shown) = both(&src, true);
        assert_eq!(json, src);
        // Node's depth limit: what is printed stays small.
        assert_eq!(shown, "[ [ [ [Array] ] ] ]");
    }

    /// As `node -e 'console.log(JSON.parse(...))'` prints them.
    #[test]
    fn inspect_applies_nodes_limits() {
        let src = r#"{"a":{"b":{"c":{"d":1},"e":[1],"f":[],"g":{}}}}"#;
        assert_eq!(
            both(src, true).1,
            "{ a: { b: { c: [Object], e: [Array], f: [], g: {} } } }"
        );
        let items = |n: usize| {
            let list: Vec<String> = (0..n).map(|i| i.to_string()).collect();
            format!("[{}]", list.join(","))
        };
        let shown = |n: usize| both(&items(n), true).1;
        assert!(shown(100).ends_with(" 98, 99 ]"));
        assert!(shown(101).ends_with(" 98, 99, ... 1 more item ]"));
        assert!(shown(1000).ends_with(" 98, 99, ... 900 more items ]"));
        // Objects show every key; JSON text is never limited.
        let keys: Vec<String> = (0..150).map(|i| format!(r#""k{i}":0"#)).collect();
        let src = format!("{{{}}}", keys.join(","));
        assert!(both(&src, true).1.ends_with("k148: 0, k149: 0 }"));
        assert_eq!(both(&items(1000), true).0, items(1000));
    }
}
