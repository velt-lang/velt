//! Runtime number formatting (`console.log`, templates, `JSON.stringify` all use `push_f64`).

/// `push_f64` prints exactly what JavaScript's `String(x)` prints, and the text parses back to
/// the same number (`-0` prints `0`, NaN/Infinity are words).
pub fn check(data: &[u8]) {
    let mut bits = [0u8; 8];
    for (i, b) in data.iter().take(8).enumerate() {
        bits[i] = *b;
    }
    let x = f64::from_bits(u64::from_le_bytes(bits));
    let mut out = Vec::new();
    velt_rt::fmt::push_f64(&mut out, x);
    let got = String::from_utf8(out).expect("push_f64 writes ASCII");
    assert_eq!(got, js_string(x), "String({x:e}) (bits {:#x})", x.to_bits());
    if x.is_finite() {
        let back: f64 = got.parse().expect("finite output parses");
        assert!(back == x, "{got} does not round-trip to {x:e}");
    }
}

/// Reference: ECMAScript Number::toString(x) (radix 10), built from Rust's shortest round-trip
/// digits (`{:e}`), which are the digits the spec asks for.
pub fn js_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if x < 0.0 {
        return format!("-{}", js_string(-x));
    }
    let (mut digits, mut exp) = sci_digits(&format!("{x:e}"));
    // Rust breaks a tie between two shortest candidates upward; the spec picks the even one.
    // A tie means x's exact expansion continues with exactly "5" after the k digits.
    let (exact, exact_exp) = sci_digits(&format!("{x:.1100e}"));
    let exact = exact.trim_end_matches('0');
    let k = digits.len();
    if exact.len() == k + 1 && exact.ends_with('5') && exact_exp == exp {
        let down = &exact[..k];
        if (down.as_bytes()[k - 1] - b'0').is_multiple_of(2) {
            digits = down.to_string();
            exp = exact_exp;
        }
    }
    let k = digits.len() as i64;
    let n = exp + 1;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let sign = if e >= 0 { "+" } else { "-" };
        let frac = if k == 1 {
            String::new()
        } else {
            format!(".{}", &digits[1..])
        };
        format!("{}{frac}e{sign}{}", &digits[..1], e.abs())
    }
}

/// Significant digits (no dot) and decimal exponent of Rust's `{:e}` output.
fn sci_digits(sci: &str) -> (String, i64) {
    let (mantissa, exp) = sci.split_once('e').expect("`{:e}` has an exponent");
    let digits = mantissa.chars().filter(|c| *c != '.').collect();
    (digits, exp.parse().expect("exponent is an integer"))
}

#[cfg(test)]
mod tests {
    use super::js_string;

    #[test]
    fn reference_matches_known_js_output() {
        let cases = [
            (0.1 + 0.2, "0.30000000000000004"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (123.456, "123.456"),
            (-2.5e-10, "-2.5e-10"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (-0.0, "0"),
            // 641914005178481.25, exactly halfway between two shortest candidates: the even
            // digit wins.
            (f64::from_bits(0x4302_3e8a_020a_438a), "641914005178481.2"),
        ];
        for (x, want) in cases {
            assert_eq!(js_string(x), want);
        }
    }

    #[test]
    fn runtime_matches_on_samples() {
        for bits in [
            0u64,
            1,
            0x3ff0_0000_0000_0000,
            0x7ff8_0000_0000_0000,
            0x4340_0000_0000_0001,
        ] {
            super::check(&bits.to_le_bytes());
        }
    }
}
