//! Text of values that carry no payload bits: literal types (`TyKind::Literal`, zero-sized —
//! the type is the value) and string enum members (an `I64` discriminant whose text is the
//! member's `VariantDef::str_value`). Used by the formatter (`console.log`, `${x}`);
//! `JSON.stringify` writes them in std's generated writers.

use velt_sema::hir::{IntTy, LitValue, TyId, TyKind};

use crate::lower::FnLower;
use crate::vir::{self, Operand, Place, Terminator};

impl FnLower<'_, '_> {
    /// The literal type of union variant `v` of `ty`, if that member is a literal type.
    pub(in crate::lower) fn variant_literal(&mut self, ty: TyId, v: u32) -> Option<LitValue> {
        let parts = self.cx.variant_tys(ty, v);
        match parts.as_slice() {
            [t] => match self.cx.kind(*t) {
                TyKind::Literal(l) => Some(l),
                _ => None,
            },
            _ => None,
        }
    }

    /// Append the console text of a literal value (strings quoted when `nested`, like node).
    pub(in crate::lower) fn push_literal(&mut self, buf: &Operand, l: &LitValue, nested: bool) {
        match l {
            LitValue::Str(s) if nested => self.push_text(buf, &inspect_quote(s)),
            LitValue::Str(s) => self.push_text(buf, s),
            LitValue::Bool(b) => self.push_text(buf, if *b { "true" } else { "false" }),
            LitValue::Int(it, n) => self.push_text(buf, &int_literal_text(*it, *n)),
            // Its text now: `String(x)` (`-0` is `0`).
            LitValue::Float(_, bits) => {
                self.push_text(buf, &velt_syntax::js_number(f64::from_bits(*bits)))
            }
        }
    }

    /// The member strings of string enum type `ty` (`None` for other types).
    pub(in crate::lower) fn enum_strings(&self, ty: TyId) -> Option<Vec<String>> {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return None;
        };
        let velt_sema::hir::Def::Enum(e) = self.cx.hir.def(d) else {
            return None;
        };
        e.variants
            .iter()
            .map(|v| v.str_value.clone())
            .collect::<Option<Vec<String>>>()
            .filter(|vs| !vs.is_empty())
    }

    /// Append the string of the string-enum value at `place` (its discriminant is the member
    /// index): quoted when `nested`.
    pub(in crate::lower) fn push_enum_str(
        &mut self,
        buf: &Operand,
        place: &Place,
        strings: &[String],
        nested: bool,
    ) {
        let join = self.new_block();
        let blocks: Vec<vir::BlockId> = strings.iter().map(|_| self.new_block()).collect();
        let cases = blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (i as i128, *b))
            .collect();
        self.terminate(Terminator::Switch {
            value: Operand::Copy(place.clone()),
            cases,
            default: join,
        });
        for (s, b) in strings.iter().zip(blocks) {
            self.switch_to(b);
            self.push_literal(buf, &LitValue::Str(s.clone()), nested);
            self.goto(join);
        }
        self.switch_to(join);
    }
}

/// The text of the value `n` of an integer literal type of width `it`. An unsuffixed literal
/// type (`i64`, as sema gives it) is a `number`'s: `String(n)` of the double it denotes, as Node
/// (`9007199254740993` is `9007199254740992`, `2^63 - 1` is `9223372036854776000`, `1e21` is
/// `1e+21`), the digits themselves wherever the double holds the value exactly below 10^21. A
/// literal of another width (`18446744073709551615u64`, `7u8`) is that integer: its digits.
pub(in crate::lower) fn int_literal_text(it: IntTy, n: i128) -> String {
    match it {
        IntTy::I64 => velt_syntax::js_number(n as f64),
        _ => n.to_string(),
    }
}

/// An object key as `console.log` shows it: bare when it is an identifier of ASCII letters,
/// digits and `_` (node quotes `$` and non-ASCII keys), else quoted (the runtime's
/// `velt_rt_strbuf_push_inspect_key` does the same at run time).
pub(crate) fn inspect_key(s: &str) -> String {
    let b = s.as_bytes();
    let ident = b
        .first()
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_');
    if ident {
        s.to_string()
    } else {
        inspect_quote(s)
    }
}

/// `s` quoted and escaped the way `console.log` shows a string inside a container (node's
/// `util.inspect`; the runtime's `velt_rt_strbuf_push_inspect_str` does the same at run time).
pub(crate) fn inspect_quote(s: &str) -> String {
    let quote = if !s.contains('\'') {
        '\''
    } else if !s.contains('"') {
        '"'
    } else if !s.contains('`') && !s.contains("${") {
        '`'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            _ if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\0'..='\u{1f}' | '\u{7f}'..='\u{9f}' => out.push_str(&format!("\\x{:02X}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::{inspect_key, int_literal_text, IntTy};

    /// An unsuffixed integer literal type: a `number`'s.
    fn number(n: i128) -> String {
        int_literal_text(IntTy::I64, n)
    }

    #[test]
    fn integer_literals_from_1e21_print_as_numbers_do() {
        // The double nearest to it is 1e21.
        assert_eq!(number(999_999_999_999_999_999_999), "1e+21");
        assert_eq!(number(123_456_789_012_345_678_901), "123456789012345680000");
        assert_eq!(number(1_000_000_000_000_000_000_000), "1e+21");
        assert_eq!(number(-1_000_000_000_000_000_000_000), "-1e+21");
    }

    /// Between 2^53 and 2^64 a `number` literal is the double nearest to it, as Node prints it.
    #[test]
    fn integer_literals_past_2_53_print_their_double() {
        assert_eq!(number(9_007_199_254_740_992), "9007199254740992");
        assert_eq!(number(9_007_199_254_740_993), "9007199254740992");
        assert_eq!(number(i64::MAX as i128), "9223372036854776000");
        assert_eq!(number(i64::MIN as i128), "-9223372036854776000");
        assert_eq!(number(42), "42");
        assert_eq!(number(-7), "-7");
    }

    /// A literal type of another width is that integer: its digits.
    #[test]
    fn sized_integer_literals_print_their_digits() {
        let max = u64::MAX as i128;
        assert_eq!(int_literal_text(IntTy::U64, max), "18446744073709551615");
        assert_eq!(int_literal_text(IntTy::USize, max), "18446744073709551615");
        assert_eq!(int_literal_text(IntTy::I32, -2147483648), "-2147483648");
        assert_eq!(int_literal_text(IntTy::U8, 255), "255");
    }

    #[test]
    fn keys_bare_only_when_identifiers_like_node() {
        for bare in ["a", "_x1", "A_9"] {
            assert_eq!(inspect_key(bare), bare);
        }
        for (key, shown) in [("$", "'$'"), ("$a", "'$a'"), ("1a", "'1a'"), ("é", "'é'")] {
            assert_eq!(inspect_key(key), shown);
        }
        assert_eq!(inspect_key("a b"), "'a b'");
        assert_eq!(inspect_key(""), "''");
    }
}
