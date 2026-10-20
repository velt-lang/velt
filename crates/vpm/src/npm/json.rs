//! `package.json` and `tsconfig.json` as data: a JSON value that keeps its object keys in the
//! order written (the order of an `exports` object's conditions decides which one wins), and
//! the JSON-with-comments that `tsconfig.json` files are.

use std::fmt;

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// A JSON value whose objects keep their keys in source order.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Parse `text` (strict JSON).
    pub fn parse(text: &str) -> Result<Json, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    /// Parse JSON with comments and trailing commas, as `tsconfig.json` allows.
    pub fn parse_jsonc(text: &str) -> Result<Json, String> {
        Json::parse(&strip_jsonc(text))
    }

    /// The value of key `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Json, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Json;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E>(self) -> Result<Json, E> {
                Ok(Json::Null)
            }
            fn visit_none<E>(self) -> Result<Json, E> {
                Ok(Json::Null)
            }
            fn visit_bool<E>(self, b: bool) -> Result<Json, E> {
                Ok(Json::Bool(b))
            }
            fn visit_i64<E>(self, n: i64) -> Result<Json, E> {
                Ok(Json::Num(n as f64))
            }
            fn visit_u64<E>(self, n: u64) -> Result<Json, E> {
                Ok(Json::Num(n as f64))
            }
            fn visit_f64<E>(self, n: f64) -> Result<Json, E> {
                Ok(Json::Num(n))
            }
            fn visit_str<E>(self, s: &str) -> Result<Json, E> {
                Ok(Json::Str(s.to_string()))
            }
            fn visit_string<E>(self, s: String) -> Result<Json, E> {
                Ok(Json::Str(s))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
                let mut items = vec![];
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Json::Arr(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
                let mut fields: Vec<(String, Json)> = vec![];
                while let Some((k, v)) = map.next_entry::<String, Json>()? {
                    // A repeated key replaces the earlier value, as in `JSON.parse`.
                    match fields.iter_mut().find(|(key, _)| *key == k) {
                        Some(slot) => slot.1 = v,
                        None => fields.push((k, v)),
                    }
                }
                Ok(Json::Obj(fields))
            }
        }
        d.deserialize_any(V)
    }
}

/// `text` without `//` and `/* */` comments and without commas before `]` or `}` (outside
/// strings), so a strict JSON parser reads it.
fn strip_jsonc(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut start = 0;
    let flush = |out: &mut String, from: usize, to: usize| out.push_str(&text[from..to]);
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                flush(&mut out, start, i);
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                start = i;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                flush(&mut out, start, i);
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                start = i;
            }
            _ => i += 1,
        }
    }
    flush(&mut out, start.min(b.len()), b.len());
    drop_trailing_commas(&out)
}

/// `text` (JSON without comments) without the commas that only whitespace separates from a
/// closing `]` or `}`.
fn drop_trailing_commas(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut start = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b',' => {
                let next = b[i + 1..].iter().find(|c| !c.is_ascii_whitespace());
                if matches!(next, Some(b']' | b'}')) {
                    out.push_str(&text[start..i]);
                    start = i + 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    out.push_str(&text[start.min(b.len())..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_key_order() {
        let j = Json::parse(r#"{"types": "a", "import": "b", "default": "c"}"#).unwrap();
        let Json::Obj(fields) = j else { panic!() };
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["types", "import", "default"]);
    }

    #[test]
    fn reads_jsonc() {
        let text = r#"{
            // a comment
            "compilerOptions": { "outDir": "./dist", /* inline */ "rootDir": "./src", },
            "include": ["src/**/*.ts", "//not a comment",],
        }"#;
        let j = Json::parse_jsonc(text).unwrap();
        let opts = j.get("compilerOptions").unwrap();
        assert_eq!(opts.get("outDir").unwrap().as_str(), Some("./dist"));
        assert_eq!(opts.get("rootDir").unwrap().as_str(), Some("./src"));
        assert_eq!(
            j.get("include"),
            Some(&Json::Arr(vec![
                Json::Str("src/**/*.ts".into()),
                Json::Str("//not a comment".into())
            ]))
        );
    }
}
