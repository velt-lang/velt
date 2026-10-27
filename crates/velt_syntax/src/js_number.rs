//! JavaScript's `Number::toString`, for property names written as number literals
//! (`{ 1.50: v }` has the key `"1.5"`) and for sema's literal types.

/// A JavaScript number as `String(n)` spells it (Number::toString: the shortest digits that
/// round-trip, in exponent form from 1e21 and below 1e-6).
pub fn js_number(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n == 0.0 {
        return "0".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let sign = if n < 0.0 { "-" } else { "" };
    // Rust's `{:e}` gives the shortest round-trip digits: `d[.ddd]e<exp>`.
    let sci = format!("{:e}", n.abs());
    let (mantissa, exp) = sci.split_once('e').expect("ICE: `{:e}` has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i64;
    let e: i64 = exp.parse().expect("ICE: `{:e}` exponent");
    let point = e + 1; // JavaScript's `n`: the decimal point after this many digits
    let body = if k <= point && point <= 21 {
        format!("{digits}{}", "0".repeat((point - k) as usize))
    } else if 0 < point && point <= 21 {
        format!(
            "{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let exp_sign = if point - 1 < 0 { "-" } else { "+" };
        let frac = match k {
            1 => String::new(),
            _ => format!(".{}", &digits[1..]),
        };
        format!("{}{frac}e{exp_sign}{}", &digits[..1], (point - 1).abs())
    };
    format!("{sign}{body}")
}
