//! The format specifiers of `console.log("%s=%d", k, v)` (#3662), as Node's `util.format`
//! reads them (`formatWithOptionsInternal` in lib/internal/util/inspect.js): the one splitter
//! behind sema, which converts each argument for its specifier (`body/expr/console_format.rs`),
//! and velt_vir's console lowering, which writes the pieces (`lower/console_format.rs`).
//!
//! Node's rules, which [`split`] follows byte for byte (`%` and the letters are ASCII, so a
//! UTF-8 string splits where Node's UTF-16 one does):
//! - `%s %d %i %f %j %o %O %c` take the next argument; with none left, they stay as written;
//! - `%%` is one `%`, with arguments left or not;
//! - any other `%x` stays as written and takes nothing, and so does a `%` at the end;
//! - the arguments no specifier took follow, each after a space.

/// A piece of a formatted line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Piece<'a> {
    /// Text written as it is.
    Text(&'a str),
    /// Argument `index` (1 for the one after the format string) as specifier `spec` (`b's'`,
    /// `b'd'`, …, `b'c'`) converts it.
    Arg(u8, usize),
}

/// A format string split for its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split<'a> {
    /// The line up to the arguments no specifier took.
    pub pieces: Vec<Piece<'a>>,
    /// How many arguments the specifiers took: arguments `used + 1 ..= args` follow, each after
    /// a space.
    pub used: usize,
}

/// The JavaScript built-ins whose `toString` Node's `%s` does not call (`hasBuiltInToString`:
/// the constructor names of V8's globals): an object whose `toString` comes from one of them is
/// inspected instead (a `Date`, an `Error`, an array). std/prelude/console_format.velt has the
/// same list.
pub const BUILTIN_CLASSES: &[&str] = &[
    "AggregateError",
    "Array",
    "ArrayBuffer",
    "BigInt",
    "BigInt64Array",
    "BigUint64Array",
    "Boolean",
    "DataView",
    "Date",
    "Error",
    "EvalError",
    "FinalizationRegistry",
    "Float32Array",
    "Float64Array",
    "Function",
    "Int16Array",
    "Int32Array",
    "Int8Array",
    "Iterator",
    "Map",
    "Number",
    "Object",
    "Promise",
    "RangeError",
    "ReferenceError",
    "RegExp",
    "Set",
    "SharedArrayBuffer",
    "String",
    "Symbol",
    "SyntaxError",
    "TypeError",
    "URIError",
    "Uint16Array",
    "Uint32Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "WeakMap",
    "WeakRef",
    "WeakSet",
];

/// The text of a format string known when compiling: the printed first argument `e` (of type
/// kind `kind`) is a string literal, or of a string literal type. Sema and the lowering ask
/// this of the same expression, so they agree on which lines are formatted when compiling.
pub fn literal_text(e: &crate::hir::Expr, kind: &crate::hir::TyKind) -> Option<String> {
    use crate::hir::{ExprKind, Lit, LitValue, TyKind};
    match (&e.kind, kind) {
        (ExprKind::Lit(Lit::Str(s)), _) | (_, TyKind::Literal(LitValue::Str(s))) => Some(s.clone()),
        _ => None,
    }
}

/// Is `b` a specifier that takes an argument?
pub fn takes_arg(b: u8) -> bool {
    matches!(b, b's' | b'd' | b'i' | b'f' | b'j' | b'o' | b'O' | b'c')
}

/// `fmt` followed by `args` more arguments, split as Node formats it; `None` when formatting
/// changes nothing (no specifier took an argument and there is no `%%`), so the line is
/// printed as without specifiers.
pub fn split(fmt: &str, args: usize) -> Option<Split<'_>> {
    let b = fmt.as_bytes();
    if !b.contains(&b'%') {
        return None;
    }
    let mut pieces = vec![];
    let (mut used, mut last, mut i) = (0, 0, 0);
    while i + 1 < b.len() {
        if b[i] == b'%' {
            i += 1;
            let c = b[i];
            if c == b'%' {
                // One `%`: the text up to and with the first.
                pieces.push(Piece::Text(&fmt[last..i]));
                last = i + 1;
            } else if used < args && takes_arg(c) {
                used += 1;
                if last != i - 1 {
                    pieces.push(Piece::Text(&fmt[last..i - 1]));
                }
                pieces.push(Piece::Arg(c, used));
                last = i + 1;
            }
        }
        i += 1;
    }
    if last == 0 {
        return None;
    }
    if last < b.len() {
        pieces.push(Piece::Text(&fmt[last..]));
    }
    pieces.retain(|p| *p != Piece::Text(""));
    Some(Split { pieces, used })
}

#[cfg(test)]
mod tests {
    use super::Piece::{Arg, Text};
    use super::*;

    fn pieces(fmt: &str, args: usize) -> Option<(Vec<Piece<'_>>, usize)> {
        split(fmt, args).map(|s| (s.pieces, s.used))
    }

    #[test]
    fn specifiers_take_arguments_in_order() {
        assert_eq!(
            pieces("%s=%d!", 2),
            Some((vec![Arg(b's', 1), Text("="), Arg(b'd', 2), Text("!")], 2))
        );
        for c in "sdifjoOc".bytes() {
            let f = format!("a%{}b", c as char);
            let got = split(&f, 1).map(|s| s.pieces);
            assert_eq!(got, Some(vec![Text("a"), Arg(c, 1), Text("b")]), "{f}");
        }
    }

    #[test]
    fn adjacent_specifiers() {
        assert_eq!(
            pieces("%s%d%i", 3),
            Some((vec![Arg(b's', 1), Arg(b'd', 2), Arg(b'i', 3)], 3))
        );
    }

    #[test]
    fn missing_arguments_stay_as_written() {
        assert_eq!(
            pieces("%s and %s", 1),
            Some((vec![Arg(b's', 1), Text(" and %s")], 1))
        );
        // Nothing taken, nothing escaped: printed as without formatting.
        assert_eq!(pieces("%s", 0), None);
    }

    #[test]
    fn excess_arguments_are_left_over() {
        assert_eq!(pieces("%s", 3), Some((vec![Arg(b's', 1)], 1)));
    }

    #[test]
    fn percent_percent() {
        assert_eq!(pieces("100%%", 1), Some((vec![Text("100%")], 0)));
        assert_eq!(
            pieces("%%s %s", 1),
            Some((vec![Text("%"), Text("s "), Arg(b's', 1)], 1))
        );
        // With no argument left, `%%` is still one `%`.
        assert_eq!(
            pieces("%d%%%%", 1),
            Some((vec![Arg(b'd', 1), Text("%"), Text("%")], 1))
        );
    }

    #[test]
    fn unknown_specifiers_and_a_trailing_percent() {
        assert_eq!(pieces("%x %y", 2), None);
        assert_eq!(pieces("50%", 1), None);
        assert_eq!(
            pieces("%x%s%", 1),
            Some((vec![Text("%x"), Arg(b's', 1), Text("%")], 1))
        );
        // `%` then a letter that is not a specifier, then a specifier: `%%s` is not read
        // as `%` + `%s`.
        assert_eq!(pieces("%q%s", 1), Some((vec![Text("%q"), Arg(b's', 1)], 1)));
    }

    #[test]
    fn non_ascii() {
        assert_eq!(
            pieces("é%sü%€", 1),
            Some((vec![Text("é"), Arg(b's', 1), Text("ü%€")], 1))
        );
        assert_eq!(pieces("%é", 1), None);
    }

    #[test]
    fn no_percent() {
        assert_eq!(pieces("plain", 2), None);
    }
}
