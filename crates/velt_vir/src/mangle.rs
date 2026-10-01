//! Symbol mangling for user functions.
//!
//! Scheme (`_V` prefix, injective, linker-safe `[A-Za-z0-9_]` only):
//! - The fully qualified HIR name is split into segments at `::` (encoded `N`), `.` (encoded `M`)
//!   and `/` (encoded `P`).
//! - Every segment is escaped: ASCII alphanumerics stay as-is, `_` becomes `__`, any other byte
//!   (and a leading digit) becomes `_` + two lowercase hex digits.
//! - Each escaped segment is emitted as `<decimal length><escaped text>`; separators sit between.
//!
//! Examples: `main` → `_V4main`, `User.greet` → `_V4UserM5greet`, `std/fs::readFile` →
//! `_V3stdP2fsN8readFile`, `main::{closure#0}` → `_V4mainN17_7bclosure_230_7d`.
//! Because escaped text never starts with a digit, decoding is unambiguous, so distinct names always
//! produce distinct symbols. Runtime symbols start with `velt_rt_`, so they can never collide.
//!
//! Generic instances append `_T` and one more segment holding their type arguments as written
//! (`sum<i64>` → `_V3sum_T3i64`); glue names its type the same way (`_Gdrop_5Point`). Symbols
//! never contain `TyId`s, so they stay the same across unrelated edits (`velt dev` hot swap
//! matches functions across versions by symbol).

/// Mangle a fully qualified HIR function name (`FnDef::name`) into a linker symbol.
pub fn mangle(name: &str) -> String {
    let mut out = String::from("_V");
    let mut seg = String::new();
    let mut rest = name;
    while !rest.is_empty() {
        let (sep, len) = if rest.starts_with("::") {
            (Some('N'), 2)
        } else if rest.starts_with('.') {
            (Some('M'), 1)
        } else if rest.starts_with('/') {
            (Some('P'), 1)
        } else {
            (None, 0)
        };
        match sep {
            Some(c) => {
                flush(&mut out, &seg);
                seg.clear();
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                let ch = rest.chars().next().unwrap();
                seg.push(ch);
                rest = &rest[ch.len_utf8()..];
            }
        }
    }
    flush(&mut out, &seg);
    out
}

fn flush(out: &mut String, seg: &str) {
    out.push_str(&segment(seg));
}

/// One length-prefixed, escaped segment (`Point` → `5Point`, `i64, string` → `15i64_2c_20string`).
pub fn segment(seg: &str) -> String {
    let mut esc = String::new();
    for (i, b) in seg.bytes().enumerate() {
        match b {
            b'0'..=b'9' if i == 0 => esc.push_str(&format!("_{b:02x}")),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => esc.push(b as char),
            b'_' => esc.push_str("__"),
            _ => esc.push_str(&format!("_{b:02x}")),
        }
    }
    format!("{}{esc}", esc.len())
}

/// The readable name of a symbol for debuggers: a mangled `_V` prefix decoded back to the HIR
/// name, type arguments in `<…>`, then any other suffix verbatim (`User.greet`, `id<i64>`,
/// `main$poll`). Other symbols (glue, runtime, `velt_main`) and undecodable ones are returned
/// unchanged.
pub fn demangle(symbol: &str) -> String {
    let Some(mut rest) = symbol.strip_prefix("_V") else {
        return symbol.to_string();
    };
    let mut name = Vec::new();
    loop {
        let Some((seg, after)) = read_segment(rest) else {
            return symbol.to_string();
        };
        name.extend(seg);
        rest = after;
        let sep = match rest.as_bytes().first() {
            Some(b'N') => "::",
            Some(b'M') => ".",
            Some(b'P') => "/",
            _ => break,
        };
        name.extend(sep.bytes());
        rest = &rest[1..];
    }
    if let Some((args, after)) = rest.strip_prefix("_T").and_then(read_segment) {
        name.push(b'<');
        name.extend(args);
        name.push(b'>');
        rest = after;
    }
    String::from_utf8_lossy(&name).into_owned() + rest
}

/// A length-prefixed escaped segment at the start of `text`: its bytes and the rest.
fn read_segment(text: &str) -> Option<(Vec<u8>, &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let len: usize = text[..digits].parse().ok()?;
    let seg = unescape(text.get(digits..digits + len)?)?;
    Some((seg, &text[digits + len..]))
}

/// Inverse of the segment escaping in `flush`.
fn unescape(esc: &str) -> Option<Vec<u8>> {
    let b = esc.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'_' {
            out.push(b[i]);
            i += 1;
        } else if b.get(i + 1) == Some(&b'_') {
            out.push(b'_');
            i += 2;
        } else {
            let hex = esc.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{demangle, mangle, segment};

    #[test]
    fn demangle_round_trips() {
        for name in [
            "main",
            "User.greet",
            "std/fs::readFile",
            "main::{closure#0}",
            "is_even",
            "3a",
            "\u{e9}t\u{e9}",
        ] {
            assert_eq!(demangle(&mangle(name)), name);
        }
        assert_eq!(demangle("_V2idM3get_T3i64"), "id.get<i64>");
        assert_eq!(
            demangle("_V3sum_T15i64_2c_20string$poll"),
            "sum<i64, string>$poll"
        );
        assert_eq!(demangle("_V2id_T9broken"), "id_T9broken");
        assert_eq!(demangle("_V4main$poll"), "main$poll");
        assert_eq!(demangle("velt_main"), "velt_main");
        assert_eq!(demangle("_V9broken"), "_V9broken");
    }

    #[test]
    fn examples() {
        assert_eq!(mangle("main"), "_V4main");
        assert_eq!(mangle("User.greet"), "_V4UserM5greet");
        assert_eq!(mangle("std/fs::readFile"), "_V3stdP2fsN8readFile");
        assert_eq!(mangle("main::{closure#0}"), "_V4mainN17_7bclosure_230_7d");
        assert_eq!(mangle("is_even"), "_V8is__even");
        assert_eq!(mangle("3a"), "_V4_33a");
        assert_eq!(segment("i64, string"), "15i64_2c_20string");
        assert_eq!(segment("Point"), "5Point");
    }

    #[test]
    fn distinct() {
        let names = [
            "a.b", "a::b", "a/b", "a_b", "a__b", "ab", "a_2eb", "a.b.c", "a.bc", "ab.c",
        ];
        let mut syms: Vec<String> = names.iter().map(|n| mangle(n)).collect();
        syms.sort();
        syms.dedup();
        assert_eq!(syms.len(), names.len());
        for s in &syms {
            assert!(
                s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "{s}"
            );
        }
    }
}
