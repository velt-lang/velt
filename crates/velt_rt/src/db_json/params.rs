//! Query parameters from JSON: the text std produces with `JSON.stringify(params)`.
//!
//! Accepted shapes: an object (named parameters: `{"id":1,"name":"a"}`), an array (positional
//! parameters: `[1,"a"]`), a single scalar (one positional parameter), or empty input (no
//! parameters). Values map as: integer literal in i64 range → [`DbValue::Int`] (exact, also
//! beyond 2^53); any other number → [`DbValue::Float`]; string → [`DbValue::Text`]; `true` /
//! `false` → [`DbValue::Bool`]; `null` → [`DbValue::Null`]; an array of integers 0–255 (what
//! `JSON.stringify` makes of a `u8[]`) → [`DbValue::Bytes`]. Nested objects are rejected.
//!
//! A batch (one statement run several times) takes a JSON array of such parameter sets
//! ([`parse_param_list`]): `[[1,"a"],[2,"b"]]` or `[{"id":1},{"id":2}]`.
//!
//! Strings without escapes borrow the source; only escaped strings allocate.

use crate::json::scan::{number_f64, number_i64, Scanner, StrTok, SyntaxError};
use std::borrow::Cow;

/// One parameter value.
#[derive(Debug, Clone, PartialEq)]
pub enum DbValue<'a> {
    /// `null`.
    Null,
    /// `true` / `false` (drivers without a boolean type bind 1 / 0).
    Bool(bool),
    /// An integer literal that fits in i64.
    Int(i64),
    /// Any other number (fraction, exponent, or out of i64 range).
    Float(f64),
    /// A string (UTF-8).
    Text(Cow<'a, str>),
    /// An array of byte values (a `u8[]`): a blob / bytea.
    Bytes(Vec<u8>),
}

/// The parameters of one statement execution.
#[derive(Debug, Clone, PartialEq)]
pub enum Params<'a> {
    /// Empty input: the statement takes no parameters.
    None,
    /// A JSON array (or a lone scalar): values for `?1, ?2, …` in order.
    Positional(Vec<DbValue<'a>>),
    /// A JSON object: values by name (without the `:` / `@` / `$` prefix), in document order.
    Named(Vec<(Cow<'a, str>, DbValue<'a>)>),
}

impl<'a> Params<'a> {
    /// The value named `name` (the last one if the object repeats a key, like `JSON.parse`).
    pub fn named(&self, name: &str) -> Option<&DbValue<'a>> {
        match self {
            Params::Named(pairs) => pairs.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Parse `src` (see the module docs). Errors are human-readable messages.
pub fn parse_params(src: &[u8]) -> Result<Params<'_>, String> {
    let mut sc = Scanner::new(src);
    let params = match sc.peek_non_ws() {
        None => return Ok(Params::None),
        Some(b'{') => Params::Named(object(&mut sc)?),
        Some(b'[') => Params::Positional(array(&mut sc)?),
        Some(_) => Params::Positional(vec![value(&mut sc)?]),
    };
    if sc.peek_non_ws().is_some() {
        return Err(syntax(sc.error("unexpected trailing characters")));
    }
    Ok(params)
}

/// Parse a JSON array of parameter sets, one per execution of a batch (each element as
/// [`parse_params`] reads a whole input; a scalar element is one positional parameter).
pub fn parse_param_list(src: &[u8]) -> Result<Vec<Params<'_>>, String> {
    let mut sc = Scanner::new(src);
    if sc.peek_non_ws() != Some(b'[') {
        return Err("batch parameters must be an array of parameter sets".to_string());
    }
    sc.pos += 1;
    let mut sets = Vec::new();
    if sc.peek_non_ws() == Some(b']') {
        sc.pos += 1;
    } else {
        loop {
            let i = sets.len();
            let set = match sc.peek_non_ws() {
                Some(b'{') => object(&mut sc).map(Params::Named),
                Some(b'[') => array(&mut sc).map(Params::Positional),
                _ => value(&mut sc).map(|v| Params::Positional(vec![v])),
            };
            sets.push(set.map_err(|e| format!("parameter set {}: {e}", i + 1))?);
            match sc.peek_non_ws() {
                Some(b',') => sc.pos += 1,
                Some(b']') => {
                    sc.pos += 1;
                    break;
                }
                _ => return Err(syntax(sc.error("expected ',' or ']'"))),
            }
        }
    }
    if sc.peek_non_ws().is_some() {
        return Err(syntax(sc.error("unexpected trailing characters")));
    }
    Ok(sets)
}

fn syntax(e: SyntaxError) -> String {
    format!("invalid parameter JSON: {} (byte {})", e.what, e.at)
}

/// `{ "key": value, ... }` with the opening brace next.
fn object<'a>(sc: &mut Scanner<'a>) -> Result<Vec<(Cow<'a, str>, DbValue<'a>)>, String> {
    sc.pos += 1;
    let mut pairs = Vec::new();
    if sc.peek_non_ws() == Some(b'}') {
        sc.pos += 1;
        return Ok(pairs);
    }
    loop {
        if sc.peek_non_ws() != Some(b'"') {
            return Err(syntax(sc.error("expected string key")));
        }
        let key = string(sc)?;
        if sc.peek_non_ws() != Some(b':') {
            return Err(syntax(sc.error("expected ':'")));
        }
        sc.pos += 1;
        let v = value(sc).map_err(|e| format!("parameter \"{key}\": {e}"))?;
        pairs.push((key, v));
        match sc.peek_non_ws() {
            Some(b',') => sc.pos += 1,
            Some(b'}') => {
                sc.pos += 1;
                return Ok(pairs);
            }
            _ => return Err(syntax(sc.error("expected ',' or '}'"))),
        }
    }
}

/// `[ value, ... ]` with the opening bracket next.
fn array<'a>(sc: &mut Scanner<'a>) -> Result<Vec<DbValue<'a>>, String> {
    sc.pos += 1;
    let mut items = Vec::new();
    if sc.peek_non_ws() == Some(b']') {
        sc.pos += 1;
        return Ok(items);
    }
    loop {
        let i = items.len();
        items.push(value(sc).map_err(|e| format!("parameter {}: {e}", i + 1))?);
        match sc.peek_non_ws() {
            Some(b',') => sc.pos += 1,
            Some(b']') => {
                sc.pos += 1;
                return Ok(items);
            }
            _ => return Err(syntax(sc.error("expected ',' or ']'"))),
        }
    }
}

/// One parameter value (a scalar or a byte array).
fn value<'a>(sc: &mut Scanner<'a>) -> Result<DbValue<'a>, String> {
    let literal = |sc: &mut Scanner<'a>, word: &[u8], v: DbValue<'a>| {
        sc.literal(word).map(|_| v).map_err(syntax)
    };
    match sc.peek_non_ws() {
        Some(b'"') => Ok(DbValue::Text(string(sc)?)),
        Some(b'-' | b'0'..=b'9') => number(sc),
        Some(b't') => literal(sc, b"true", DbValue::Bool(true)),
        Some(b'f') => literal(sc, b"false", DbValue::Bool(false)),
        Some(b'n') => literal(sc, b"null", DbValue::Null),
        Some(b'[') => bytes(sc),
        Some(b'{') => Err("nested objects cannot be bound as parameters".to_string()),
        _ => Err(syntax(sc.unexpected())),
    }
}

fn number<'a>(sc: &mut Scanner<'a>) -> Result<DbValue<'a>, String> {
    let tok = sc.number().map_err(syntax)?;
    if tok.integer {
        if let Some(i) = number_i64(sc.src, tok) {
            return Ok(DbValue::Int(i));
        }
    }
    Ok(DbValue::Float(number_f64(sc.src, tok)))
}

fn string<'a>(sc: &mut Scanner<'a>) -> Result<Cow<'a, str>, String> {
    let src = sc.src;
    let bytes: Cow<'a, [u8]> = match sc.string(true).map_err(syntax)? {
        StrTok::Borrowed(start, end) => Cow::Borrowed(&src[start..end]),
        StrTok::Owned(v) => Cow::Owned(v),
    };
    // The source is a Velt string (UTF-8) and decoded escapes are UTF-8, so this only fails
    // for input that did not come from `JSON.stringify`.
    match bytes {
        Cow::Borrowed(b) => std::str::from_utf8(b).map(Cow::Borrowed),
        Cow::Owned(v) => String::from_utf8(v)
            .map(Cow::Owned)
            .map_err(|e| e.utf8_error()),
    }
    .map_err(|_| "parameter string is not valid UTF-8".to_string())
}

/// `[0, 255, ...]`: a byte array (blob).
fn bytes<'a>(sc: &mut Scanner<'a>) -> Result<DbValue<'a>, String> {
    sc.pos += 1;
    let mut out = Vec::new();
    if sc.peek_non_ws() == Some(b']') {
        sc.pos += 1;
        return Ok(DbValue::Bytes(out));
    }
    loop {
        let byte = match sc.peek_non_ws() {
            Some(b'-' | b'0'..=b'9') => match number(sc)? {
                DbValue::Int(i) => u8::try_from(i).ok(),
                _ => None,
            },
            _ => None,
        };
        let Some(byte) = byte else {
            return Err(
                "arrays bind as blobs and must hold only integers 0-255 (a u8[])".to_string(),
            );
        };
        out.push(byte);
        match sc.peek_non_ws() {
            Some(b',') => sc.pos += 1,
            Some(b']') => {
                sc.pos += 1;
                return Ok(DbValue::Bytes(out));
            }
            _ => return Err(syntax(sc.error("expected ',' or ']'"))),
        }
    }
}
