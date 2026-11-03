//! The storage floats' fast path (`lower/half.rs` `half_fast`, #2191): one `+ - * /` of two
//! `f16` (or two `bf16`) values, or the square root of one, computed in f32 and rounded to the
//! format gives what computing in f64 and rounding once gives, which is what JavaScript's
//! `Float16Array` stores. Rounding twice is innocuous because f32 has at least 2p + 2 bits
//! (Figueroa 1995); this checks it against the reference conversions, NaN results comparing as
//! NaN. The square root runs every pattern; the sampled pair test runs every 251st left operand
//! against every right one; the exhaustive one (every pair, about two minutes) is
//! `cargo test -p velt_vir --release --test half_fast -- --ignored`.

use velt_vir::math::half;

#[derive(Clone, Copy, Debug)]
enum Format {
    F16,
    Bf16,
}

fn widen(f: Format, h: u16) -> f32 {
    f32::from_bits(match f {
        Format::F16 => half::f16_to_f32(h),
        Format::Bf16 => half::bf16_to_f32(h),
    })
}

fn round32(f: Format, x: f32) -> u16 {
    match f {
        Format::F16 => half::f32_to_f16(x.to_bits()),
        Format::Bf16 => half::f32_to_bf16(x.to_bits()),
    }
}

fn round64(f: Format, x: f64) -> u16 {
    match f {
        Format::F16 => half::f64_to_f16(x.to_bits()),
        Format::Bf16 => half::f64_to_bf16(x.to_bits()),
    }
}

fn same(f: Format, a: u16, b: u16) -> bool {
    let inf = match f {
        Format::F16 => 0x7c00,
        Format::Bf16 => 0x7f80,
    };
    a == b || (a & 0x7fff > inf && b & 0x7fff > inf)
}

/// The pairs `(a, b)` with `a` in `lefts`, every `b`, where the fast path differs (at most 5).
fn differences(f: Format, lefts: impl Iterator<Item = u16>) -> Vec<String> {
    let mut bad = vec![];
    for a in lefts {
        let x = widen(f, a);
        let xd = f64::from(x);
        for b in 0..=u16::MAX {
            let y = widen(f, b);
            let yd = f64::from(y);
            let fast = [x + y, x - y, x * y, x / y];
            let exact = [xd + yd, xd - yd, xd * yd, xd / yd];
            for (k, op) in ["+", "-", "*", "/"].iter().enumerate() {
                let (got, want) = (round32(f, fast[k]), round64(f, exact[k]));
                if !same(f, got, want) && bad.len() < 5 {
                    bad.push(format!(
                        "{f:?} {a:#06x} {op} {b:#06x}: {got:#06x}, want {want:#06x}"
                    ));
                }
            }
        }
    }
    bad
}

#[test]
fn one_operation_in_f32_rounds_as_f64_does_sampled() {
    for f in [Format::F16, Format::Bf16] {
        let bad = differences(f, (0..=u16::MAX).step_by(251));
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

#[test]
#[ignore = "every pair: about two minutes in release"]
fn one_operation_in_f32_rounds_as_f64_does_exhaustive() {
    for f in [Format::F16, Format::Bf16] {
        let bad = std::thread::scope(|s| {
            let parts: Vec<_> = (0..16u16)
                .map(|t| s.spawn(move || differences(f, (t..=u16::MAX).step_by(16))))
                .collect();
            parts
                .into_iter()
                .flat_map(|p| p.join().expect("a part"))
                .collect::<Vec<_>>()
        });
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// The patterns whose square root in f32, rounded to the format, differs from the f64 square
/// root rounded once (at most 5), and how many patterns gave a NaN, an infinity or a zero and
/// how many are subnormals of the format (so the sweep is seen to reach each kind).
fn sqrt_differences(f: Format) -> (Vec<String>, [u32; 4]) {
    let mut bad = vec![];
    let mut kinds = [0; 4];
    let exponent = match f {
        Format::F16 => 0x7c00,
        Format::Bf16 => 0x7f80,
    };
    for a in 0..=u16::MAX {
        let x = widen(f, a);
        let (got, want) = (round32(f, x.sqrt()), round64(f, f64::from(x).sqrt()));
        if !same(f, got, want) && bad.len() < 5 {
            bad.push(format!("{f:?} sqrt {a:#06x}: {got:#06x}, want {want:#06x}"));
        }
        let r = widen(f, want);
        kinds[0] += u32::from(r.is_nan());
        kinds[1] += u32::from(r.is_infinite());
        kinds[2] += u32::from(r == 0.0);
        kinds[3] += u32::from(a & exponent == 0 && a & 0x7fff != 0);
    }
    (bad, kinds)
}

/// Every pattern of both formats: negatives give NaN, -0 gives -0, and the subnormals' roots
/// are normal, all as the f64 root rounded once.
#[test]
fn sqrt_in_f32_rounds_as_f64_does_exhaustive() {
    for f in [Format::F16, Format::Bf16] {
        let (bad, [nans, infs, zeros, subnormals]) = sqrt_differences(f);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        assert!(
            nans > 32_000 && infs == 1 && zeros == 2 && subnormals > 0,
            "{f:?}"
        );
    }
}
