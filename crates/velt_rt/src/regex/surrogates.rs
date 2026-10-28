//! Surrogate code units in patterns (#1745).
//!
//! The `regex` crate matches code points and has no spelling for a surrogate. A pattern that
//! names one (`\uD800`, `[\uD800-\uDFFF]`) is translated in surrogate mode: each set of
//! surrogates becomes an alternation of the byte sequences of their WTF-8 form (`ED A0..BF
//! 80..BF`) in a `(?-u:…)` group, next to the class of the other members. In that mode `.`, the
//! negated shorthands (`\D \W \S \P{…}`) and negated classes match surrogates too, as in JS.
//!
//! What a surrogate is matched against depends on the `u` flag. With it, the subject is its
//! WTF-8 bytes as they are: a pair is one 4-byte code point, so only a lone surrogate matches
//! (`/[\uD800-\uDFFF]/u` does not match `"😀"`), and `\uD83D\uDE00` is the code point U+1F600.
//! Without it, JS matches code units: the subject is searched in its "units" form, where a pair
//! is written as its two 3-byte surrogates (mod.rs), so `[\uD800-\uDFFF]` matches either half
//! of `"😀"` and `.` matches one half. The pattern's own supplementary characters are then two
//! code units as well ([`prepare`]), so `/😀+/` repeats the low half, as in JS.

use std::borrow::Cow;
use std::iter::Peekable;
use std::str::Chars;

use crate::str::wtf8;

type Input<'a> = Peekable<Chars<'a>>;

const FIRST: u32 = 0xD800;
const LAST: u32 = 0xDFFF;

fn is_surrogate(v: u32) -> bool {
    (FIRST..=LAST).contains(&v)
}

/// The pattern text of `bytes` (WTF-8) for the translation: a lone surrogate becomes a `\uXXXX`
/// escape, and without `unicode` a supplementary character becomes its two code units' escapes.
/// A character right after a backslash that escapes it gets the escape's `u…` only (`\` + U+D800
/// is the identity escape of that unit, `\uD800`).
pub fn prepare(bytes: &[u8], unicode: bool) -> Cow<'_, str> {
    let astral = !unicode && bytes.iter().any(|&b| b >= 0xF0);
    if !astral && wtf8::count_lone(bytes) == 0 {
        return wtf8::to_utf8_lossy(bytes);
    }
    let mut out = String::with_capacity(bytes.len() + 16);
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let (cp, len) = wtf8::decode_at(bytes, i);
        i += len;
        let units = match cp {
            FIRST..=LAST => [cp, 0],
            0x10000.. if !unicode => {
                let v = cp - 0x10000;
                [0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF)]
            }
            _ => {
                // Any code point but a surrogate is a `char`.
                out.push(char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER));
                escaped = cp == u32::from(b'\\') && !escaped;
                continue;
            }
        };
        for u in units.into_iter().filter(|&u| u != 0) {
            out.push_str(if escaped { "u" } else { "\\u" });
            out.push_str(&format!("{u:04X}"));
            escaped = false;
        }
    }
    Cow::Owned(out)
}

/// Does `js` name a surrogate code unit: a `\uXXXX` escape of one (with `unicode`, also
/// `\u{…}`), or a class whose members reach into the surrogates (`[\0-\uFFFF]`, or a negated
/// class with a member right next to them, `[^\0-\uD7FF]`)? Then it is translated in surrogate
/// mode.
pub fn mentions(js: &str, unicode: bool) -> bool {
    names_unit(js, unicode) || has_reaching_class(js, unicode)
}

/// Does `js` hold a `\\u` escape of a surrogate (even one half of an escaped pair)?
fn names_unit(js: &str, unicode: bool) -> bool {
    let mut chars = js.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' || chars.next() != Some('u') {
            continue;
        }
        let v = match chars.peek() {
            Some('{') if unicode => braced(&mut chars),
            _ => hex(&mut chars, 4),
        };
        if v.is_some_and(is_surrogate) {
            return true;
        }
    }
    false
}

/// Does `js` hold a class that reaches into the surrogates ([`class_reaches`])?
fn has_reaching_class(js: &str, unicode: bool) -> bool {
    let mut chars = js.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' => {
                let negated = chars.next_if_eq(&'^').is_some();
                if chars.next_if_eq(&']').is_none() && class_reaches(&mut chars, negated, unicode) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// After a class's `[` (and `^`): does a member or range of it reach into the surrogates, or end
/// right next to them (`[^\0-\uD7FF]`: the negation then holds them)? Consumes the class.
fn class_reaches(chars: &mut Input<'_>, negated: bool, unicode: bool) -> bool {
    let next_to = negated as u32;
    let touches = |a: u32, b: u32| a <= LAST + next_to && b + next_to >= FIRST;
    let mut reaches = false;
    loop {
        let first = match atom(chars, unicode) {
            Ok(Atom::End) | Err(_) => return reaches,
            Ok(a) => a,
        };
        let mut look = chars.clone();
        let range = look.next() == Some('-') && !matches!(look.peek(), None | Some(']'));
        let second = match range {
            true => {
                *chars = look;
                match atom(chars, unicode) {
                    Ok(Atom::End) | Err(_) => return reaches,
                    Ok(b) => Some(b),
                }
            }
            false => None,
        };
        match (first, second) {
            (Atom::Value(a), Some(Atom::Value(b))) => reaches |= touches(a, b),
            (Atom::Value(a), None) => reaches |= touches(a, a),
            _ => {}
        }
    }
}

/// `n` hex digits, consumed only when all are there.
fn hex(chars: &mut Input<'_>, n: usize) -> Option<u32> {
    let mut look = chars.clone();
    let mut v = 0;
    for _ in 0..n {
        v = v * 16 + look.next()?.to_digit(16)?;
    }
    *chars = look;
    Some(v)
}

/// `{…}` of a `\u{…}` escape (a code point), consumed only when well-formed.
fn braced(chars: &mut Input<'_>) -> Option<u32> {
    let mut look = chars.clone();
    look.next();
    let digits: String = look.clone().take_while(|&c| c != '}').collect();
    let v = u32::from_str_radix(&digits, 16)
        .ok()
        .filter(|&v| v <= 0x10FFFF)?;
    if digits.is_empty() || look.nth(digits.chars().count()) != Some('}') {
        return None;
    }
    *chars = look;
    Some(v)
}

/// After `\u`: the code unit of `\uXXXX`, or with `unicode` the code point of a `\u{…}` or of a
/// pair of escapes (`\uD83D\uDE00`); consumed only when well-formed.
fn escape_u(chars: &mut Input<'_>, unicode: bool) -> Option<u32> {
    if unicode && chars.peek() == Some(&'{') {
        return braced(chars);
    }
    let v = hex(chars, 4)?;
    if unicode && (0xD800..0xDC00).contains(&v) {
        let mut look = chars.clone();
        if look.next() == Some('\\') && look.next() == Some('u') {
            if let Some(lo) = hex(&mut look, 4).filter(|lo| (0xDC00..=LAST).contains(lo)) {
                *chars = look;
                return Some(0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00));
            }
        }
    }
    Some(v)
}

/// Every surrogate, as a regex.
pub fn all() -> String {
    Units(vec![(FIRST, LAST)]).regex()
}

/// The translation of `\u…` outside a class in surrogate mode (`None`: not well-formed).
pub fn unit_escape(chars: &mut Input<'_>, unicode: bool) -> Option<String> {
    let v = escape_u(chars, unicode)?;
    Some(match is_surrogate(v) {
        true => Units(vec![(v, v)]).regex(),
        false => format!("\\x{{{v:X}}}"),
    })
}

/// A set of surrogate code units, as inclusive ranges.
#[derive(Default)]
struct Units(Vec<(u32, u32)>);

impl Units {
    /// Sorted, with overlapping and adjacent ranges merged.
    fn normalized(mut self) -> Units {
        self.0.sort_unstable();
        let mut out: Vec<(u32, u32)> = Vec::with_capacity(self.0.len());
        for (a, b) in self.0 {
            match out.last_mut() {
                Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
                _ => out.push((a, b)),
            }
        }
        Units(out)
    }

    /// The surrogates not in this (normalized) set.
    fn complement(&self) -> Units {
        let mut out = Vec::new();
        let mut next = FIRST;
        for &(a, b) in &self.0 {
            if a > next {
                out.push((next, a - 1));
            }
            next = b + 1;
        }
        if next <= LAST {
            out.push((next, LAST));
        }
        Units(out)
    }

    /// The WTF-8 byte sequences of the set as one group (`""` for an empty set). A surrogate
    /// `U` is `ED`, `80 | (U >> 6) & 3F`, `80 | U & 3F`.
    fn regex(&self) -> String {
        let mid = |u: u32| 0x80 | ((u >> 6) & 0x3F);
        let low = |u: u32| 0x80 | (u & 0x3F);
        let mut alts = Vec::new();
        for &(a, b) in &self.0 {
            let (a1, a2, b1, b2) = (mid(a), low(a), mid(b), low(b));
            if a1 == b1 {
                alts.push(format!(r"\xED\x{a1:02X}[\x{a2:02X}-\x{b2:02X}]"));
                continue;
            }
            alts.push(format!(r"\xED\x{a1:02X}[\x{a2:02X}-\xBF]"));
            if a1 + 1 < b1 {
                alts.push(format!(
                    r"\xED[\x{:02X}-\x{:02X}][\x80-\xBF]",
                    a1 + 1,
                    b1 - 1
                ));
            }
            alts.push(format!(r"\xED\x{b1:02X}[\x80-\x{b2:02X}]"));
        }
        if alts.is_empty() {
            return String::new();
        }
        format!("(?-u:{})", alts.join("|"))
    }
}

/// One member of a class.
enum Atom {
    /// A character or code unit (a range can end at it).
    Value(u32),
    /// A set (`\d`, `\p{L}`, …): its translation, and whether it holds every surrogate.
    Set(String, bool),
    /// An escape passed on as it is.
    Text(String),
    /// The closing `]`.
    End,
}

const UNTERMINATED: &str = "Unterminated character class";

fn atom(chars: &mut Input<'_>, unicode: bool) -> Result<Atom, String> {
    match chars.next() {
        None => Err(UNTERMINATED.to_string()),
        Some(']') => Ok(Atom::End),
        Some('\\') => {
            let e = chars
                .next()
                .ok_or_else(|| "\\ at end of pattern".to_string())?;
            Ok(class_escape(e, chars, unicode))
        }
        Some(c) => Ok(Atom::Value(c as u32)),
    }
}

fn class_escape(e: char, chars: &mut Input<'_>, unicode: bool) -> Atom {
    let value = match e {
        'd' | 'w' | 's' | 'D' | 'W' | 'S' => {
            let mut text = String::new();
            super::syntax::push_escape(&mut text, e, true);
            return Atom::Set(text, e.is_ascii_uppercase());
        }
        'p' | 'P' if unicode && chars.peek() == Some(&'{') => {
            let name: String = chars.take_while(|&c| c != '}').collect();
            return Atom::Set(format!("\\{e}{name}}}"), e == 'P');
        }
        'b' => 8,
        't' => 9,
        'n' => 10,
        'v' => 11,
        'f' => 12,
        'r' => 13,
        '0' if !chars.peek().is_some_and(char::is_ascii_digit) => 0,
        'c' if chars.peek().is_some_and(char::is_ascii_alphabetic) => {
            chars.next().map_or(0, |c| c as u32 % 32)
        }
        'x' => hex(chars, 2).unwrap_or('x' as u32),
        'u' => escape_u(chars, unicode).unwrap_or('u' as u32),
        _ if !e.is_ascii_alphanumeric() => e as u32,
        _ if !unicode && e.is_ascii_alphabetic() => e as u32,
        _ => return Atom::Text(format!("\\{e}")),
    };
    Atom::Value(value)
}

/// A class's members: the translation of the members other than surrogates, and the
/// surrogates.
#[derive(Default)]
struct Members {
    text: String,
    units: Units,
}

impl Members {
    fn push(&mut self, a: Atom) {
        match a {
            Atom::Value(v) => self.range(v, v),
            Atom::Set(text, all) => {
                self.text.push_str(&text);
                if all {
                    self.units.0.push((FIRST, LAST));
                }
            }
            Atom::Text(text) => self.text.push_str(&text),
            Atom::End => {}
        }
    }

    /// `a-b`, split at the surrogates.
    fn range(&mut self, a: u32, b: u32) {
        let mut part = |lo: u32, hi: u32| {
            if lo == hi {
                self.text.push_str(&format!("\\x{{{lo:X}}}"));
            } else if lo < hi {
                self.text.push_str(&format!("\\x{{{lo:X}}}-\\x{{{hi:X}}}"));
            }
        };
        if a < FIRST {
            part(a, b.min(FIRST - 1));
        }
        if b > LAST {
            part(a.max(LAST + 1), b);
        }
        if a <= LAST && b >= FIRST {
            self.units.0.push((a.max(FIRST), b.min(LAST)));
        }
    }
}

/// A class in surrogate mode, after its `[` (and `^` when `negated`) up to and including its
/// `]`; the class is not empty (`[]`, `[^]` are the caller's).
pub fn class(chars: &mut Input<'_>, negated: bool, unicode: bool) -> Result<String, String> {
    let mut m = Members::default();
    loop {
        let first = atom(chars, unicode)?;
        if matches!(first, Atom::End) {
            break;
        }
        let mut look = chars.clone();
        if look.next() != Some('-') || matches!(look.peek(), None | Some(']')) {
            m.push(first);
            continue;
        }
        *chars = look;
        match (first, atom(chars, unicode)?) {
            (Atom::Value(a), Atom::Value(b)) if a > b => {
                return Err("Range out of order in character class".to_string())
            }
            (Atom::Value(a), Atom::Value(b)) => m.range(a, b),
            // Annex B: a set at either end makes the `-` a member (`[\d-z]`).
            (a, b) => {
                m.push(a);
                m.text.push_str("\\-");
                m.push(b);
            }
        }
    }
    let units = std::mem::take(&mut m.units).normalized();
    let (base, units) = match negated {
        false if m.text.is_empty() => (String::new(), units),
        false => (format!("[{}]", m.text), units),
        true if m.text.is_empty() => ("(?s:.)".to_string(), units.complement()),
        true => (format!("[^{}]", m.text), units.complement()),
    };
    Ok(match (base.is_empty(), units.0.is_empty()) {
        (true, true) => "[^\\x00-\\x{10FFFF}]".to_string(),
        (false, true) => base,
        (true, false) => units.regex(),
        (false, false) => format!("(?:{base}|{})", units.regex()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surrogate_ranges_as_bytes() {
        assert_eq!(
            all(),
            r"(?-u:\xED\xA0[\x80-\xBF]|\xED[\xA1-\xBE][\x80-\xBF]|\xED\xBF[\x80-\xBF])"
        );
        let one = Units(vec![(0xD83D, 0xD83D)]).regex();
        assert_eq!(one, r"(?-u:\xED\xA0[\xBD-\xBD])");
        let comp = Units(vec![(0xD800, 0xDBFF)]).normalized().complement();
        assert_eq!(comp.0, [(0xDC00, 0xDFFF)]);
    }

    #[test]
    fn mentions_surrogate_escapes() {
        assert!(mentions(r#"["\\\x00-\x1f\uD800-\uDFFF]"#, false));
        assert!(mentions(r"\udc00", false));
        assert!(mentions(r"[\x00-\uFFFF]", false));
        assert!(mentions(r"a[\uD7FF-\uE000]", true));
        assert!(mentions(r"[^\^@-\uD7FF]", false));
        assert!(!mentions(r"[@-\uD7FF]", false));
        assert!(!mentions(r"\[\x00-\uFFFF]", false));
        assert!(!mentions(r"[^a-z]+[\uE001-\uFFFF]", false));
        assert!(mentions(r"\u{D800}", true));
        assert!(!mentions(r"\u{D800}", false));
        assert!(!mentions(r"\u0041\x{D800}", false));
    }

    #[test]
    fn prepare_escapes_lone_and_astral_units() {
        let lone = [b'a', 0xED, 0xA0, 0x80];
        assert_eq!(prepare(&lone, true), r"a\uD800");
        assert_eq!(prepare("😀+".as_bytes(), false), r"\uD83D\uDE00+");
        assert_eq!(prepare("😀+".as_bytes(), true), "😀+");
        assert_eq!(prepare(r"\😀".as_bytes(), false), r"\uD83D\uDE00");
        assert_eq!(prepare(r"\\😀".as_bytes(), false), r"\\\uD83D\uDE00");
    }

    fn class_of(src: &str, unicode: bool) -> Result<String, String> {
        let mut chars = src.chars().peekable();
        let negated = chars.next_if_eq(&'^').is_some();
        class(&mut chars, negated, unicode)
    }

    #[test]
    fn classes_split_at_the_surrogates() {
        let c = class_of(r"a\uD800-\uDBFF]", false).expect("valid");
        assert!(c.starts_with(r"(?:[\x{61}]|(?-u:"), "{c}");
        let c = class_of(r"\x00-\uFFFF]", false).expect("valid");
        assert!(
            c.starts_with(r"(?:[\x{0}-\x{D7FF}\x{E000}-\x{FFFF}]|"),
            "{c}"
        );
        assert_eq!(class_of(r"^\uD800-\uDFFF]", true).expect("valid"), "(?s:.)");
        assert!(class_of(r"\uDFFF-\uD800]", false).is_err());
        assert!(class_of(r"\uD800", false).is_err());
    }
}
