//! The f16/bf16 helpers (`half.rs`) compiled with clang for the host and run on the conversions'
//! edge values (every 16-bit pattern widened; every f16 and bf16 value, its neighbours, the
//! midpoints between neighbours and one ulp either side, narrowed from f32 and f64; integers at
//! every power of two and the bf16 midpoints there), against `velt_vir::math::half`, bit for
//! bit. Both variants run where the host CPU has the instructions: the integer sequences
//! everywhere, `fcvt` on arm64 and F16C on an x86 CPU with it. Skipped (with a note) without
//! clang.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use velt_vir::math::{half, MathOp};

use crate::half::{helper, HalfNative};

/// The inputs: f16/bf16 patterns, f32 bits, f64 bits and integers.
struct Inputs {
    h: Vec<u16>,
    f: Vec<u32>,
    d: Vec<u64>,
    i: Vec<u64>,
}

fn inputs() -> Inputs {
    let h: Vec<u16> = (0..=u16::MAX).collect();
    let (mut f, mut d) = (vec![], vec![]);
    for &p in &h {
        for w in [half::f16_to_f32(p), half::bf16_to_f32(p)] {
            let v = f64::from(f32::from_bits(w)).to_bits();
            for delta in [0u64, 1, u64::MAX] {
                d.push(v.wrapping_add(delta));
                for unit in [1u64 << 41, 1 << 44] {
                    d.push(v.wrapping_add(unit).wrapping_add(delta));
                    d.push(v.wrapping_sub(unit).wrapping_add(delta));
                }
                f.push(w.wrapping_add(delta as u32));
                for unit in [1u32 << 12, 1 << 15] {
                    f.push(w.wrapping_add(unit).wrapping_add(delta as u32));
                    f.push(w.wrapping_sub(unit).wrapping_add(delta as u32));
                }
            }
        }
    }
    // The ties of the subnormal grids, (2m+1)·2^-25 (f16) and (2m+1)·2^-134 (bf16), and one
    // ulp either side: the magic-addition path's rounding.
    let ties = (0..1024).map(|m| f64::from(2 * m + 1) * 2f64.powi(-25));
    for t in ties.chain((0..128).map(|m| f64::from(2 * m + 1) * 2f64.powi(-134))) {
        for x in [t, -t] {
            let (b, w) = (x.to_bits(), (x as f32).to_bits());
            d.extend([b, b.wrapping_add(1), b.wrapping_sub(1)]);
            f.extend([w, w.wrapping_add(1), w.wrapping_sub(1)]);
        }
    }
    let mut i = vec![0, u64::MAX, i64::MAX as u64, 1 << 63];
    for e in 0..64u32 {
        for frac in [0u64, 1 << 55, 1 << 56, 3 << 55] {
            for delta in [u64::MAX, 0, 1] {
                let v = (1u64 << e)
                    .wrapping_add(frac >> (63 - e))
                    .wrapping_add(delta);
                i.extend([v, v.wrapping_neg()]);
            }
        }
    }
    Inputs { h, f, d, i }
}

/// The conversions: the op, the helper's argument and result types in IR, the input set.
const OPS: [(MathOp, &str, &str, char); 8] = [
    (MathOp::F16ToF32, "i16", "float", 'h'),
    (MathOp::Bf16ToF32, "i16", "float", 'h'),
    (MathOp::F32ToF16, "float", "i16", 'f'),
    (MathOp::F32ToBf16, "float", "i16", 'f'),
    (MathOp::F64ToF16, "double", "i16", 'd'),
    (MathOp::F64ToBf16, "double", "i16", 'd'),
    (MathOp::U64ToBf16, "i64", "i16", 'i'),
    (MathOp::I64ToBf16, "i64", "i16", 'i'),
];

/// What `velt_vir::math::half` gives for input `x` (bits) of `op`, zero-extended.
fn reference(op: MathOp, x: u64) -> u64 {
    match op {
        MathOp::F16ToF32 => u64::from(half::f16_to_f32(x as u16)),
        MathOp::Bf16ToF32 => u64::from(half::bf16_to_f32(x as u16)),
        MathOp::F32ToF16 => u64::from(half::f32_to_f16(x as u32)),
        MathOp::F32ToBf16 => u64::from(half::f32_to_bf16(x as u32)),
        MathOp::F64ToF16 => u64::from(half::f64_to_f16(x)),
        MathOp::F64ToBf16 => u64::from(half::f64_to_bf16(x)),
        MathOp::U64ToBf16 => u64::from(half::u64_to_bf16(x)),
        MathOp::I64ToBf16 => u64::from(half::i64_to_bf16(x as i64)),
        _ => unreachable!("ICE: not a conversion"),
    }
}

/// A module with every helper of `native` and an exported `w<k>` calling the `k`th op's.
fn module(native: HalfNative) -> String {
    let mut defs = std::collections::BTreeSet::new();
    let mut ir = String::new();
    for (k, (op, arg, ret, _)) in OPS.iter().enumerate() {
        let (name, ds) = helper(*op, native).expect("a conversion");
        defs.extend(ds.iter().map(|d| d.to_string()));
        // The C ABI's extension of 16-bit values, which C reads them by.
        let zext = *ret == "i16";
        let r = if zext {
            "zeroext i16".to_string()
        } else {
            ret.to_string()
        };
        let a = if *arg == "i16" { "i16 zeroext" } else { arg };
        let _ = writeln!(
            ir,
            "define {r} @w{k}({a} %x) {{\n  %r = call {ret} {name}({arg} %x)\n  ret {ret} %r\n}}"
        );
    }
    defs.into_iter().fold(ir, |mut s, d| {
        s.push_str(&d);
        s.push('\n');
        s
    })
}

/// The C driver: reads the input file, runs every wrapper over its input set, writes the
/// results (zero-extended to 64 bits) in order.
const DRIVER: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
float w0(uint16_t); float w1(uint16_t); uint16_t w2(float); uint16_t w3(float);
uint16_t w4(double); uint16_t w5(double); uint16_t w6(uint64_t); uint16_t w7(uint64_t);
static uint64_t *read_set(FILE *in, uint64_t *n) {
  if (fread(n, 8, 1, in) != 1) exit(2);
  uint64_t *xs = malloc(*n * 8 + 8);
  if (fread(xs, 8, *n, in) != *n) exit(2);
  return xs;
}
int main(int argc, char **argv) {
  FILE *in = fopen(argv[1], "rb"), *out = fopen(argv[2], "wb");
  uint64_t nh, nf, nd, ni;
  uint64_t *h = read_set(in, &nh), *f = read_set(in, &nf), *d = read_set(in, &nd), *i = read_set(in, &ni);
  for (int k = 0; k < 8; k++) {
    uint64_t n = k < 2 ? nh : k < 4 ? nf : k < 6 ? nd : ni;
    uint64_t *xs = k < 2 ? h : k < 4 ? f : k < 6 ? d : i;
    for (uint64_t j = 0; j < n; j++) {
      uint64_t r = 0, x = xs[j];
      float fv; double dv; uint32_t fb = (uint32_t)x;
      memcpy(&fv, &fb, 4); memcpy(&dv, &x, 8);
      switch (k) {
        case 0: { float y = w0((uint16_t)x); uint32_t b; memcpy(&b, &y, 4); r = b; break; }
        case 1: { float y = w1((uint16_t)x); uint32_t b; memcpy(&b, &y, 4); r = b; break; }
        case 2: r = w2(fv); break;
        case 3: r = w3(fv); break;
        case 4: r = w4(dv); break;
        case 5: r = w5(dv); break;
        case 6: r = w6(x); break;
        case 7: r = w7(x); break;
      }
      fwrite(&r, 8, 1, out);
    }
  }
  fclose(out);
  return 0;
}
"#;

fn write_set(out: &mut Vec<u8>, xs: impl ExactSizeIterator<Item = u64>) {
    out.extend((xs.len() as u64).to_le_bytes());
    for x in xs {
        out.extend(x.to_le_bytes());
    }
}

/// Compiles `native`'s helpers with `flags`, runs them on `inputs` and compares.
fn run(clang: &Path, dir: &Path, native: HalfNative, flags: &[&str], inputs: &Inputs) {
    let tag = format!("{native:?}");
    let ll = dir.join(format!("{tag}.ll"));
    let c = dir.join("driver.c");
    let exe = dir.join(format!("{tag}.exe"));
    std::fs::write(&ll, module(native)).expect("write .ll");
    std::fs::write(&c, DRIVER).expect("write driver");
    let built = Command::new(clang)
        .args(["-O2", "-w", "-o"])
        .arg(&exe)
        .args(flags)
        .arg(&ll)
        .arg(&c)
        .output()
        .expect("run clang");
    let err = String::from_utf8_lossy(&built.stderr);
    assert!(built.status.success(), "{tag}: clang failed:\n{err}");
    let (input, output) = (dir.join("in.bin"), dir.join(format!("{tag}.out")));
    let mut bytes = vec![];
    write_set(&mut bytes, inputs.h.iter().map(|&x| u64::from(x)));
    write_set(&mut bytes, inputs.f.iter().map(|&x| u64::from(x)));
    write_set(&mut bytes, inputs.d.iter().copied());
    write_set(&mut bytes, inputs.i.iter().copied());
    std::fs::write(&input, bytes).expect("write inputs");
    let status = Command::new(&exe)
        .arg(&input)
        .arg(&output)
        .status()
        .expect("run");
    assert!(status.success(), "{tag}: driver failed");
    let got = std::fs::read(&output).expect("read results");
    let mut got = got
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c));
    let mut bad = vec![];
    for (op, _, _, set) in OPS {
        let xs: Vec<u64> = match set {
            'h' => inputs.h.iter().map(|&x| u64::from(x)).collect(),
            'f' => inputs.f.iter().map(|&x| u64::from(x)).collect(),
            'd' => inputs.d.clone(),
            _ => inputs.i.clone(),
        };
        for x in xs {
            let (want, have) = (reference(op, x), got.next().expect("a result"));
            if want != have && bad.len() < 20 {
                bad.push(format!("{tag} {op:?}({x:#x}): {have:#x}, want {want:#x}"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn half_helpers_match_the_reference_bit_for_bit() {
    if cfg!(windows) {
        // The driver is C, compiled and linked by clang: Windows' toolchains differ, and the
        // helpers are the same IR there (Linux and macOS cover them).
        eprintln!("note: skipping the f16/bf16 helper run on Windows");
        return;
    }
    let Some(clang) = crate::find_clang() else {
        eprintln!("note: clang not available; skipping the f16/bf16 helper run");
        return;
    };
    let dir = std::env::temp_dir().join(format!("velt_half_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    // Removes the directory however the test ends.
    struct Cleanup<'a>(&'a Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0);
        }
    }
    let _cleanup = Cleanup(&dir);
    let inputs = inputs();
    run(&clang, &dir, HalfNative::None, &[], &inputs);
    if cfg!(target_arch = "aarch64") {
        run(&clang, &dir, HalfNative::All, &[], &inputs);
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("f16c") {
        run(&clang, &dir, HalfNative::F32, &["-mf16c"], &inputs);
    }
}

/// The target decides which helpers: arm64 all three f16 instructions, x86 F16C's two under a
/// `--cpu` with it, nothing on the default x86-64-v2 and wasm.
#[test]
fn half_native_follows_the_target() {
    let t = |triple: &str, cpu: Option<&str>| {
        crate::target::normalize(triple)
            .and_then(|t| t.with_cpu(cpu))
            .expect("a target")
            .half_native()
    };
    assert_eq!(t("aarch64-apple-darwin", None), HalfNative::All);
    assert_eq!(t("aarch64-unknown-linux-gnu", None), HalfNative::All);
    assert_eq!(t("x86_64-unknown-linux-gnu", None), HalfNative::None);
    assert_eq!(
        t("x86_64-unknown-linux-gnu", Some("x86-64-v3")),
        HalfNative::F32
    );
    assert_eq!(t("wasm32-wasip1", None), HalfNative::None);
    assert_eq!(
        helper(MathOp::F64ToF16, HalfNative::F32).map(|h| h.0),
        Some("@velt.f64_to_f16.soft")
    );
    assert_eq!(
        helper(MathOp::F32ToF16, HalfNative::F32).map(|h| h.0),
        Some("@velt.f32_to_f16")
    );
}
