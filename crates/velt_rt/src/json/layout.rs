//! `JSON.stringify(value, replacer, space)` with an array replacer or an indent: the compact
//! text the generated glue writes for `value`, laid out again as JS writes it. JS's output with
//! a property list and a gap differs from the compact one only outside strings (objects keep
//! just the listed keys, in the list's order; the gap adds line breaks and indentation, and
//! `": "` after keys), so walking the compact text gives exactly JS's bytes. ABI:
//! rt_abi_async.md §12.5.

use super::scan::{Scanner, SyntaxError};
use super::walk::scalar;
use crate::str::VeltStr;

/// The `text` of `JSON.stringify(value)` re-laid out: only the members of objects whose keys
/// are in `keys` (in that order; `None` keeps every member in place), and with `indent` as JS's
/// gap (empty: no whitespace). Malformed text (never written by the glue) comes back as is.
pub fn relayout(text: &[u8], keys: Option<&[Vec<u8>]>, indent: &[u8]) -> Vec<u8> {
    let mut l = Layout {
        keys,
        indent,
        out: Vec::with_capacity(text.len() + text.len() / 2),
    };
    let mut sc = Scanner::new(text);
    match l.value(&mut sc, 0) {
        Ok(()) => l.out,
        Err(_) => text.to_vec(),
    }
}

/// The keys of a replacer array from its compact JSON text (`["a","b"]`), decoded, without
/// duplicates (as JS builds its property list); `None` if the text is not an array of strings.
pub fn property_list(text: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut sc = Scanner::new(text);
    if sc.peek_non_ws() != Some(b'[') {
        return None;
    }
    sc.pos += 1;
    let mut keys: Vec<Vec<u8>> = vec![];
    if sc.peek_non_ws() == Some(b']') {
        return Some(keys);
    }
    loop {
        if sc.peek_non_ws() != Some(b'"') {
            return None;
        }
        let k = sc.string(true).ok()?.bytes(text).to_vec();
        if !keys.contains(&k) {
            keys.push(k);
        }
        match sc.peek_non_ws() {
            Some(b',') => sc.pos += 1,
            Some(b']') => return Some(keys),
            _ => return None,
        }
    }
}

struct Layout<'a> {
    keys: Option<&'a [Vec<u8>]>,
    indent: &'a [u8],
    out: Vec<u8>,
}

/// An object member: its decoded key, its key as written (with the quotes) and its value laid
/// out.
struct Member {
    key: Vec<u8>,
    raw: (usize, usize),
    value: Vec<u8>,
}

impl Layout<'_> {
    fn value(&mut self, sc: &mut Scanner, depth: usize) -> Result<(), SyntaxError> {
        match sc.peek_non_ws() {
            Some(b'{') => self.object(sc, depth),
            Some(b'[') => self.array(sc, depth),
            _ => {
                let start = sc.pos;
                scalar(sc, false)?;
                self.out.extend_from_slice(&sc.src[start..sc.pos]);
                Ok(())
            }
        }
    }

    /// A line break and the indentation of `depth` (nothing without a gap).
    fn newline(&mut self, depth: usize) {
        if self.indent.is_empty() {
            return;
        }
        self.out.push(b'\n');
        for _ in 0..depth {
            self.out.extend_from_slice(self.indent);
        }
    }

    fn array(&mut self, sc: &mut Scanner, depth: usize) -> Result<(), SyntaxError> {
        sc.pos += 1;
        if sc.peek_non_ws() == Some(b']') {
            sc.pos += 1;
            self.out.extend_from_slice(b"[]");
            return Ok(());
        }
        self.out.push(b'[');
        loop {
            self.newline(depth + 1);
            self.value(sc, depth + 1)?;
            match sc.peek_non_ws() {
                Some(b',') => {
                    sc.pos += 1;
                    self.out.push(b',');
                }
                Some(b']') => break,
                _ => return Err(sc.unexpected()),
            }
        }
        sc.pos += 1;
        self.newline(depth);
        self.out.push(b']');
        Ok(())
    }

    fn object(&mut self, sc: &mut Scanner, depth: usize) -> Result<(), SyntaxError> {
        let members = self.members(sc, depth)?;
        let chosen: Vec<&Member> = match self.keys {
            None => members.iter().collect(),
            Some(keys) => keys
                .iter()
                .filter_map(|k| members.iter().find(|m| m.key == *k))
                .collect(),
        };
        if chosen.is_empty() {
            self.out.extend_from_slice(b"{}");
            return Ok(());
        }
        self.out.push(b'{');
        for (i, m) in chosen.into_iter().enumerate() {
            if i > 0 {
                self.out.push(b',');
            }
            self.newline(depth + 1);
            self.out.extend_from_slice(&sc.src[m.raw.0..m.raw.1]);
            self.out.push(b':');
            if !self.indent.is_empty() {
                self.out.push(b' ');
            }
            self.out.extend_from_slice(&m.value);
        }
        self.newline(depth);
        self.out.push(b'}');
        Ok(())
    }

    /// The members of the object at `sc` (its `{` there), each value laid out at `depth + 1`.
    fn members(&mut self, sc: &mut Scanner, depth: usize) -> Result<Vec<Member>, SyntaxError> {
        sc.pos += 1;
        let mut members = vec![];
        if sc.peek_non_ws() == Some(b'}') {
            sc.pos += 1;
            return Ok(members);
        }
        loop {
            if sc.peek_non_ws() != Some(b'"') {
                return Err(sc.unexpected());
            }
            let start = sc.pos;
            let decode = self.keys.is_some();
            let key = sc.string(decode)?.bytes(sc.src).to_vec();
            let raw = (start, sc.pos);
            if sc.peek_non_ws() != Some(b':') {
                return Err(sc.unexpected());
            }
            sc.pos += 1;
            let outer = std::mem::take(&mut self.out);
            let laid = self.value(sc, depth + 1);
            let value = std::mem::replace(&mut self.out, outer);
            laid?;
            members.push(Member { key, raw, value });
            match sc.peek_non_ws() {
                Some(b',') => sc.pos += 1,
                Some(b'}') => {
                    sc.pos += 1;
                    return Ok(members);
                }
                _ => return Err(sc.unexpected()),
            }
        }
    }
}

/// `JSON.stringify(value, replacer, space)` from the compact `*text` of `JSON.stringify(value)`:
/// `filter` 1 keeps only the object keys listed in `*keys` (the compact JSON text of the
/// replacer's array of strings), `*indent` is the gap. Writes an owned string to `*out`.
///
/// # Safety
/// `text`, `keys` and `indent` must point to valid strings and `out` to writable memory.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_relayout(
    text: *const VeltStr,
    keys: *const VeltStr,
    filter: u8,
    indent: *const VeltStr,
    out: *mut VeltStr,
) {
    let text = (*text).as_bytes();
    let list = match filter {
        0 => None,
        _ => property_list((*keys).as_bytes()),
    };
    let laid = relayout(text, list.as_deref(), (*indent).as_bytes());
    out.write(VeltStr::from_vec(laid));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay(text: &str, keys: Option<&[&str]>, indent: &str) -> String {
        let keys: Option<Vec<Vec<u8>>> =
            keys.map(|ks| ks.iter().map(|k| k.as_bytes().to_vec()).collect());
        String::from_utf8(relayout(text.as_bytes(), keys.as_deref(), indent.as_bytes())).unwrap()
    }

    #[test]
    fn indents_like_js() {
        assert_eq!(lay("[]", None, "  "), "[]");
        assert_eq!(lay("{}", None, "  "), "{}");
        assert_eq!(lay("[1,[],{}]", None, " "), "[\n 1,\n [],\n {}\n]");
        assert_eq!(
            lay(r#"{"a":[1,2],"b":{"c":"x,{"}}"#, None, "\t"),
            "{\n\t\"a\": [\n\t\t1,\n\t\t2\n\t],\n\t\"b\": {\n\t\t\"c\": \"x,{\"\n\t}\n}"
        );
        assert_eq!(lay("\"s\"", None, "  "), "\"s\"");
    }

    #[test]
    fn keeps_listed_keys_in_list_order() {
        let t = r#"{"a":1,"b":{"a":2,"c":3},"c":[{"b":4,"a":5}]}"#;
        assert_eq!(lay(t, Some(&["c", "a"]), ""), r#"{"c":[{"a":5}],"a":1}"#);
        assert_eq!(lay(t, Some(&[]), ""), "{}");
        assert_eq!(lay(r#"{"é":1}"#, Some(&["é"]), ""), r#"{"é":1}"#);
    }

    #[test]
    fn property_list_dedups() {
        let l = property_list(br#"["a","b","a"]"#).unwrap();
        assert_eq!(l, vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(property_list(b"[]").unwrap().len(), 0);
        assert!(property_list(b"[1]").is_none());
    }
}
