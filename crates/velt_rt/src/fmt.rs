//! Number formatting shared by `velt_rt_write_*` and `velt_rt_str_from_*`.
//!
//! Floats follow ECMAScript `Number::toString(10)` exactly: shortest round-trip digits
//! (from `ryu`), laid out with the JS rules (plain notation for exponents in `-7 < e < 21`,
//! otherwise `d.ddde±x`), `NaN`, `Infinity`, `-Infinity`, and `-0` printed as `0`.

/// Append the decimal representation of `v`.
#[inline]
pub fn push_i64(out: &mut Vec<u8>, v: i64) {
    crate::freed::check_value(v as u64, "an integer read from a freed block");
    let mut b = itoa::Buffer::new();
    out.extend_from_slice(b.format(v).as_bytes());
}

/// Append the decimal representation of `v`.
#[inline]
pub fn push_u64(out: &mut Vec<u8>, v: u64) {
    let mut b = itoa::Buffer::new();
    out.extend_from_slice(b.format(v).as_bytes());
}

#[inline]
pub fn push_bool(out: &mut Vec<u8>, v: u8) {
    out.extend_from_slice(if v != 0 { b"true" } else { b"false" });
}

/// Append `v` formatted like node's `util.inspect` (what `console.log` prints): as
/// [`push_f64`], except that `-0` is `-0`.
pub fn push_inspect_f64(out: &mut Vec<u8>, v: f64) {
    if v == 0.0 && v.is_sign_negative() {
        out.extend_from_slice(b"-0");
        return;
    }
    push_f64(out, v);
}

/// `v` as an integer when it is a whole number below 2^53 in magnitude (`-0` is 0): what JS
/// prints for it is the integer's digits.
#[inline]
pub fn whole(v: f64) -> Option<i64> {
    (v.abs() < 9_007_199_254_740_992.0 && v == v.trunc()).then_some(v as i64)
}

/// Append `v` formatted like JavaScript's `String(v)`.
pub fn push_f64(out: &mut Vec<u8>, v: f64) {
    crate::freed::check_value(v.to_bits(), "a number read from a freed block");
    // A counter or an index (`${i}`): its integer digits, which is what the general path below
    // prints for it, without the shortest-digits search. `-0` prints `0` as JS does.
    if let Some(i) = whole(v) {
        push_i64(out, i);
        return;
    }
    if v.is_nan() {
        out.extend_from_slice(b"NaN");
        return;
    }
    if v.is_infinite() {
        out.extend_from_slice(if v < 0.0 { b"-Infinity" } else { b"Infinity" });
        return;
    }
    if v == 0.0 {
        // Covers -0 as well: JS prints "0".
        out.push(b'0');
        return;
    }
    if v < 0.0 {
        out.push(b'-');
    }
    let (digits, k, n) = shortest_digits(v.abs());
    let digits = &digits[..k];
    // Value = 0.d1d2..dk * 10^n  (i.e. digits * 10^(n-k)); JS spec names: k, n.
    if k as i32 <= n && n <= 21 {
        // Integer: digits followed by n-k zeros.
        out.extend_from_slice(digits);
        out.resize(out.len() + (n - k as i32) as usize, b'0');
    } else if 0 < n && n <= 21 {
        let n = n as usize;
        out.extend_from_slice(&digits[..n]);
        out.push(b'.');
        out.extend_from_slice(&digits[n..]);
    } else if -6 < n && n <= 0 {
        out.extend_from_slice(b"0.");
        out.resize(out.len() + (-n) as usize, b'0');
        out.extend_from_slice(digits);
    } else {
        out.push(digits[0]);
        if k > 1 {
            out.push(b'.');
            out.extend_from_slice(&digits[1..]);
        }
        out.push(b'e');
        let e = n - 1;
        out.push(if e < 0 { b'-' } else { b'+' });
        push_u64(out, e.unsigned_abs() as u64);
    }
}

/// Shortest round-trip decimal digits of a finite, positive `v`.
/// Returns `(digits, k, n)` with `v == 0.d1..dk * 10^n`, `d1 != 0`, `dk != 0`.
fn shortest_digits(v: f64) -> ([u8; 20], usize, i32) {
    let mut buf = ryu::Buffer::new();
    // ryu prints e.g. "1.0", "0.3", "1e21", "1.5e-7", "1.2345678901234568e20", "0.0001".
    let s = buf.format_finite(v).as_bytes();
    let (mantissa, exp) = match s.iter().position(|&c| c == b'e') {
        Some(i) => (&s[..i], parse_exp(&s[i + 1..])),
        None => (s, 0),
    };
    let mut digits = [0u8; 20];
    let mut k = 0usize;
    // Position of the decimal point relative to the first *emitted* digit.
    let mut int_len: i32 = 0;
    let mut seen_point = false;
    let mut leading = true;
    for &c in mantissa {
        if c == b'.' {
            seen_point = true;
            continue;
        }
        if leading && c == b'0' {
            if seen_point {
                int_len -= 1;
            }
            continue;
        }
        leading = false;
        if !seen_point {
            int_len += 1;
        }
        digits[k] = c;
        k += 1;
    }
    while k > 1 && digits[k - 1] == b'0' {
        k -= 1;
    }
    (digits, k, int_len + exp)
}

fn parse_exp(s: &[u8]) -> i32 {
    let (neg, s) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut e: i32 = 0;
    for &c in s {
        e = e * 10 + (c - b'0') as i32;
    }
    if neg {
        -e
    } else {
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn js(v: f64) -> String {
        let mut o = Vec::new();
        push_f64(&mut o, v);
        String::from_utf8(o).unwrap()
    }

    #[test]
    #[allow(clippy::excessive_precision, clippy::approx_constant)] // literals are deliberate
    fn js_number_to_string_table() {
        // Expected values are what V8/SpiderMonkey print for String(x).
        let table: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (10.0, "10"),
            (100.0, "100"),
            (1.5, "1.5"),
            (-1.5, "-1.5"),
            (0.5, "0.5"),
            (0.1, "0.1"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1.0 / 3.0, "0.3333333333333333"),
            (2.0 / 3.0, "0.6666666666666666"),
            (100.0 / 3.0, "33.333333333333336"),
            (123.456, "123.456"),
            (-123.456, "-123.456"),
            (12345678.9, "12345678.9"),
            (4.35, "4.35"),
            (1.0000000000000002, "1.0000000000000002"),
            (9007199254740992.0, "9007199254740992"),
            (9007199254740993.0, "9007199254740992"),
            (1e16, "10000000000000000"),
            (1e20, "100000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (999999999999999900000.0, "999999999999999900000"),
            (1e21, "1e+21"),
            (-1e21, "-1e+21"),
            (1.2345678901234568e21, "1.2345678901234568e+21"),
            (1.5e21, "1.5e+21"),
            (1e100, "1e+100"),
            (1.5e300, "1.5e+300"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (1e-6, "0.000001"),
            (1.23e-6, "0.00000123"),
            (0.000001234, "0.000001234"),
            (2.5e-5, "0.000025"),
            (0.001, "0.001"),
            (1e-7, "1e-7"),
            (-1e-7, "-1e-7"),
            (1.5e-7, "1.5e-7"),
            (1.23e-18, "1.23e-18"),
            (2.220446049250313e-16, "2.220446049250313e-16"),
            (5e-324, "5e-324"),
            (2.2250738585072014e-308, "2.2250738585072014e-308"),
            (0.1f32 as f64, "0.10000000149011612"),
            (f64::NAN, "NaN"),
            (-f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (i64::MAX as f64, "9223372036854776000"),
            (u64::MAX as f64, "18446744073709552000"),
            (255.0, "255"),
            (3.14159, "3.14159"),
            (6.02214076e23, "6.02214076e+23"),
            // Exact tie between two 17-digit candidates: JS picks the even one.
            (1658206780088562.25, "1658206780088562.2"),
        ];
        for &(v, want) in table {
            assert_eq!(js(v), want, "formatting {v:e}");
        }
    }

    /// Cross-check ryu-derived digits against Rust's own shortest `{:e}` on many bit patterns.
    #[test]
    fn digits_match_std_shortest() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let v = f64::from_bits(x & 0x7FFF_FFFF_FFFF_FFFF);
            if !v.is_finite() || v == 0.0 {
                continue;
            }
            let (d, k, n) = shortest_digits(v);
            let std = format!("{v:e}");
            let (m, e) = std.split_once('e').unwrap();
            let std_digits: String = m.chars().filter(|&c| c != '.').collect();
            let ours = std::str::from_utf8(&d[..k]).unwrap();
            if ours != std_digits {
                // Exact ties between two shortest candidates: ECMAScript (and ryu) pick the even
                // digit string, Rust's std does not. Anything else is a real mismatch.
                let (a, b) = (ours.as_bytes(), std_digits.as_bytes());
                assert!(
                    a.len() == b.len()
                        && a[..a.len() - 1] == b[..b.len() - 1]
                        && a[a.len() - 1] % 2 == 0,
                    "{std} vs ours {ours}"
                );
            }
            assert_eq!(n - 1, e.parse::<i32>().unwrap(), "{std}");
            // And the JS rendering must round-trip.
            assert_eq!(js(v).parse::<f64>().unwrap(), v);
        }
    }

    /// Compare against a corpus produced by a real JS engine:
    /// `node crates/velt_rt/tests/js_float_corpus.js > corpus.txt`, then
    /// `VELT_RT_JS_CORPUS=corpus.txt cargo test -p velt_rt --lib -- --ignored js_corpus`.
    #[test]
    #[ignore = "needs VELT_RT_JS_CORPUS generated by node"]
    fn js_corpus() {
        let path = std::env::var("VELT_RT_JS_CORPUS").expect("VELT_RT_JS_CORPUS");
        let text = std::fs::read_to_string(path).unwrap();
        let mut n = 0;
        for line in text.lines() {
            let (hex, want) = line.split_once('\t').unwrap();
            let v = f64::from_bits(u64::from_str_radix(hex, 16).unwrap());
            assert_eq!(js(v), want, "bits {hex}");
            n += 1;
        }
        assert!(n > 0);
        eprintln!("checked {n} values");
    }

    #[test]
    fn ints() {
        let mut o = Vec::new();
        push_i64(&mut o, i64::MIN);
        o.push(b' ');
        push_u64(&mut o, u64::MAX);
        o.push(b' ');
        push_bool(&mut o, 1);
        push_bool(&mut o, 0);
        assert_eq!(o, b"-9223372036854775808 18446744073709551615 truefalse");
    }
}
