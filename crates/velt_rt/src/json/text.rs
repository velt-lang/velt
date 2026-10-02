//! The two texts of a `json.Value` tree: `JSON.stringify` (`stringify_into`) and what
//! `console.log` prints (`inspect_into`, node's `util.inspect` of the parsed object). One
//! iterative walk serves both, so arbitrarily deep values cannot overflow the stack.

use super::escape::push_json_string;
use super::object::Entries;
use super::value::Value;
use crate::fmt;
use crate::inspect::{push_inspect_key, push_inspect_string};
use std::sync::Arc;

/// How a text form writes the parts of a tree.
trait Style {
    /// A string value inside a container (or at the top level, for JSON).
    fn string(out: &mut Vec<u8>, s: &str);
    /// The opening of a container with members (`[`, `{` / `[ `, `{ `).
    const OPEN: [&'static [u8]; 2];
    /// Between two members.
    const SEP: &'static [u8];
    /// The closing of a container with members.
    const CLOSE: [&'static [u8]; 2];
    /// An object key with what separates it from its value.
    fn key(out: &mut Vec<u8>, k: &str);
}

/// `JSON.stringify`: no whitespace, numbers JS-formatted (non-finite → `null`).
struct Json;

impl Style for Json {
    fn string(out: &mut Vec<u8>, s: &str) {
        push_json_string(out, s.as_bytes());
    }
    const OPEN: [&'static [u8]; 2] = [b"[", b"{"];
    const SEP: &'static [u8] = b",";
    const CLOSE: [&'static [u8]; 2] = [b"]", b"}"];
    fn key(out: &mut Vec<u8>, k: &str) {
        push_json_string(out, k.as_bytes());
        out.push(b':');
    }
}

/// `console.log`: `{ a: 1, b: [ 2, 'x' ] }`, `[]`, `{}`, like Velt's other values (one line).
struct Inspect;

impl Style for Inspect {
    fn string(out: &mut Vec<u8>, s: &str) {
        push_inspect_string(out, s.as_bytes());
    }
    const OPEN: [&'static [u8]; 2] = [b"[ ", b"{ "];
    const SEP: &'static [u8] = b", ";
    const CLOSE: [&'static [u8]; 2] = [b" ]", b" }"];
    fn key(out: &mut Vec<u8>, k: &str) {
        push_inspect_key(out, k.as_bytes());
        out.extend_from_slice(b": ");
    }
}

/// Append `JSON.stringify(v)`: key order preserved, numbers JS-formatted (non-finite → `null`).
pub fn stringify_into(out: &mut Vec<u8>, v: &Value) {
    write_tree::<Json>(out, v);
}

/// Append what `console.log` prints for `v`: a string raw at the top level (`top`), quoted
/// inside containers and when not `top` (a member of another printed value).
pub fn inspect_into(out: &mut Vec<u8>, v: &Value, top: bool) {
    match v {
        Value::String(s) if top => out.extend_from_slice(s.as_bytes()),
        _ => write_tree::<Inspect>(out, v),
    }
}

/// A container being written: the members still to write, and whether one was written.
enum Open<'a> {
    Array(std::slice::Iter<'a, Arc<Value>>),
    Object(Entries<'a>),
}

/// Write `v` in style `S`, iteratively.
fn write_tree<S: Style>(out: &mut Vec<u8>, v: &Value) {
    let mut stack: Vec<(Open, bool)> = Vec::new();
    let mut next = Some(v);
    loop {
        if let Some(v) = next.take() {
            match v {
                Value::Null => out.extend_from_slice(b"null"),
                Value::Bool(b) => fmt::push_bool(out, *b as u8),
                Value::Number(n) if n.is_finite() => fmt::push_f64(out, *n),
                Value::Number(_) => out.extend_from_slice(b"null"),
                Value::String(s) => S::string(out, s),
                Value::Array(items) if items.is_empty() => out.extend_from_slice(b"[]"),
                Value::Object(obj) if obj.is_empty() => out.extend_from_slice(b"{}"),
                Value::Array(items) => {
                    out.extend_from_slice(S::OPEN[0]);
                    stack.push((Open::Array(items.iter()), false));
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
            Open::Array(items) => items.next().map(|v| (None, v)),
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
        inspect_into(&mut shown, &v, top);
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
        assert_eq!(shown.len(), 2 * depth + 2 * (depth - 1));
    }
}
