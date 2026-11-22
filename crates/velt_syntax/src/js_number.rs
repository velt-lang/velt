//! JavaScript's `Number::toString`, for property names written as number literals
//! (`{ 1.50: v }` has the key `"1.5"`), for sema's literal types and for the text lowering
//! writes for a float literal.

/// A JavaScript number as `String(n)` spells it (Number::toString: the shortest digits that
/// round-trip, the even ones on a tie, in exponent form from 1e21 and below 1e-6), by
/// `velt_text`'s printer.
pub fn js_number(n: f64) -> String {
    let mut out = Vec::with_capacity(24);
    velt_text::fmt::push_f64(&mut out, n);
    String::from_utf8(out).expect("ICE: a number's text is ASCII")
}
