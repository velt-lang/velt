//! The Rust side of half_arith.velt, on arm64. With the feature `native` (nightly, the date in
//! half_arith.sh): `std::arch`'s f16 kernels. Without it (stable): `half`'s slice conversions to
//! and from `f32`, and `vfmaq_f32`, what a program can write on stable Rust.
#![cfg_attr(feature = "native", feature(stdarch_neon_f16))]
use half::f16;

const N: usize = 1048576;
const A: u16 = 0x1400;
const S: u16 = 0x3e00;
const B: u16 = 0x3000;

#[cfg(feature = "native")]
mod kernels {
    use super::*;
    use core::arch::aarch64::*;

    #[inline]
    #[target_feature(enable = "fp16")]
    unsafe fn load(p: *const f16) -> float16x8_t {
        vld1q_f16(p.cast())
    }

    #[target_feature(enable = "fp16")]
    #[inline(never)]
    pub unsafe fn axpy(x: &[f16], y: &mut [f16], _z: &mut [f16]) -> u64 {
        let n = x.len().min(y.len());
        let a = vreinterpretq_f16_u16(vdupq_n_u16(A));
        let mut i = 0;
        while i + 8 <= n {
            let r = vfmaq_f16(load(y.as_ptr().add(i)), a, load(x.as_ptr().add(i)));
            vst1q_f16(y.as_mut_ptr().add(i).cast(), r);
            i += 8;
        }
        0
    }

    #[target_feature(enable = "fp16")]
    #[inline(never)]
    pub unsafe fn act(x: &[f16], _y: &mut [f16], z: &mut [f16]) -> u64 {
        let n = x.len().min(z.len());
        let s = vreinterpretq_f16_u16(vdupq_n_u16(S));
        let b = vreinterpretq_f16_u16(vdupq_n_u16(B));
        let zero = vreinterpretq_f16_u16(vdupq_n_u16(0));
        let mut i = 0;
        while i + 8 <= n {
            let r = vmaxq_f16(vfmaq_f16(b, load(x.as_ptr().add(i)), s), zero);
            vst1q_f16(z.as_mut_ptr().add(i).cast(), r);
            i += 8;
        }
        0
    }

    #[target_feature(enable = "fp16")]
    #[inline(never)]
    pub unsafe fn dot(x: &[f16], y: &mut [f16], _z: &mut [f16]) -> u64 {
        let n = x.len().min(y.len());
        let zero = vreinterpretq_f16_u16(vdupq_n_u16(0));
        let mut s = [zero; 4];
        let mut i = 0;
        while i + 32 <= n {
            for (k, sum) in s.iter_mut().enumerate() {
                let (p, q) = (x.as_ptr().add(i + 8 * k), y.as_ptr().add(i + 8 * k));
                *sum = vfmaq_f16(*sum, load(p), load(q));
            }
            i += 32;
        }
        let all = vaddq_f16(vaddq_f16(s[0], s[1]), vaddq_f16(s[2], s[3]));
        let mut bits = [0u16; 8];
        vst1q_u16(bits.as_mut_ptr(), vreinterpretq_u16_f16(all));
        bits.iter().map(|&b| b as u64).sum()
    }
}

#[cfg(not(feature = "native"))]
mod kernels {
    use super::*;
    use core::arch::aarch64::*;
    use half::slice::HalfFloatSliceExt;

    #[inline]
    unsafe fn widened(p: &[f16]) -> [float32x4_t; 2] {
        let mut w = [0f32; 8];
        p.convert_to_f32_slice(&mut w);
        [vld1q_f32(w.as_ptr()), vld1q_f32(w.as_ptr().add(4))]
    }

    #[inline]
    unsafe fn narrowed(v: [float32x4_t; 2], to: &mut [f16]) {
        let mut w = [0f32; 8];
        vst1q_f32(w.as_mut_ptr(), v[0]);
        vst1q_f32(w.as_mut_ptr().add(4), v[1]);
        to.convert_from_f32_slice(&w);
    }

    #[inline(never)]
    pub unsafe fn axpy(x: &[f16], y: &mut [f16], _z: &mut [f16]) -> u64 {
        let a = vdupq_n_f32(f16::from_bits(A).to_f32());
        for (p, q) in x.chunks_exact(8).zip(y.chunks_exact_mut(8)) {
            let (u, v) = (widened(p), widened(q));
            narrowed([vfmaq_f32(v[0], a, u[0]), vfmaq_f32(v[1], a, u[1])], q);
        }
        0
    }

    #[inline(never)]
    pub unsafe fn act(x: &[f16], _y: &mut [f16], z: &mut [f16]) -> u64 {
        let s = vdupq_n_f32(f16::from_bits(S).to_f32());
        let b = vdupq_n_f32(f16::from_bits(B).to_f32());
        let zero = vdupq_n_f32(0.0);
        for (p, q) in x.chunks_exact(8).zip(z.chunks_exact_mut(8)) {
            let u = widened(p);
            let r = |v| vmaxq_f32(vfmaq_f32(b, v, s), zero);
            narrowed([r(u[0]), r(u[1])], q);
        }
        0
    }

    #[inline(never)]
    pub unsafe fn dot(x: &[f16], y: &mut [f16], _z: &mut [f16]) -> u64 {
        let mut s = [[f16::from_bits(0); 8]; 4];
        for (p, q) in x.chunks_exact(32).zip(y.chunks_exact(32)) {
            for k in 0..4 {
                let (u, v, w) = (
                    widened(&p[8 * k..8 * k + 8]),
                    widened(&q[8 * k..8 * k + 8]),
                    widened(&s[k]),
                );
                let r = [vfmaq_f32(w[0], u[0], v[0]), vfmaq_f32(w[1], u[1], v[1])];
                narrowed(r, &mut s[k]);
            }
        }
        let added = |a: &[f16; 8], b: &[f16; 8]| {
            let mut r = [f16::from_bits(0); 8];
            for k in 0..8 {
                r[k] = a[k] + b[k];
            }
            r
        };
        let all = added(&added(&s[0], &s[1]), &added(&s[2], &s[3]));
        all.iter().map(|v| v.to_bits() as u64).sum()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let usage = || -> ! {
        eprintln!("usage: half_arith <axpy|act|dot> <passes>");
        std::process::exit(2)
    };
    if args.len() != 3 {
        usage();
    }
    let passes: usize = args[2].parse().unwrap_or_else(|_| usage());
    let kernel = match args[1].as_str() {
        "axpy" => kernels::axpy,
        "act" => kernels::act,
        "dot" => kernels::dot,
        _ => usage(),
    };
    let mut x = Vec::with_capacity(N);
    let mut y = Vec::with_capacity(N);
    let mut z = vec![f16::from_bits(0); N];
    for i in 0..N {
        let sign = if i % 5 == 0 { 0x8000 } else { 0 };
        x.push(f16::from_bits(0x3000 + ((i % 97) * 31) as u16 | sign));
        y.push(f16::from_bits(0x2c00 + ((i % 89) * 37) as u16));
    }
    let mut sum = 0u64;
    for _ in 0..passes {
        sum = sum.wrapping_add(unsafe { kernel(std::hint::black_box(&x), &mut y, &mut z) });
    }
    for i in 0..N {
        sum = sum.wrapping_add(y[i].to_bits() as u64 + z[i].to_bits() as u64);
    }
    println!("{sum}");
}
