//! The Rust side of half_dot.vlt: `half` for the weights, `std::arch` for the kernels, on arm64.
//! `vfmlalq_low_f16`/`vfmlalq_high_f16` are still unstable (`RUSTC_BOOTSTRAP=1`, half_dot.sh).
//! `std::arch` has no `vbfdotq_f32` in Rust 1.99 and no `bf16` vector type, so `bfdot` is the
//! same one instruction by `asm!`. The bf16 exact kernels widen as `half` does (a shift and a
//! NaN's quiet bit) and use `vfmaq_f32`.
#![feature(stdarch_neon_f16, f16)]
use core::arch::aarch64::*;
use core::arch::asm;
use half::{bf16, f16};

struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

#[inline]
unsafe fn total(s: float32x4_t) -> f64 {
    vgetq_lane_f32::<0>(s) as f64
        + vgetq_lane_f32::<1>(s) as f64
        + vgetq_lane_f32::<2>(s) as f64
        + vgetq_lane_f32::<3>(s) as f64
}

#[inline]
#[target_feature(enable = "fhm,fp16")]
unsafe fn fmlal(s: float32x4_t, a: *const f16, b: *const f16) -> float32x4_t {
    let (x, y) = (vld1q_f16(a.cast()), vld1q_f16(b.cast()));
    vfmlalq_high_f16(vfmlalq_low_f16(s, x, y), x, y)
}

#[target_feature(enable = "fhm,fp16")]
#[inline(never)]
unsafe fn f16_one(a: &[f16], b: &[f16]) -> f64 {
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let mut s = vdupq_n_f32(0.0);
    let mut i = 0;
    while i + 8 <= n {
        s = fmlal(s, pa.add(i), pb.add(i));
        i += 8;
    }
    total(s)
}

#[target_feature(enable = "fhm,fp16")]
#[inline(never)]
unsafe fn f16_four(a: &[f16], b: &[f16]) -> f64 {
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let (mut s0, mut s1, mut s2, mut s3) = (
        vdupq_n_f32(0.0),
        vdupq_n_f32(0.0),
        vdupq_n_f32(0.0),
        vdupq_n_f32(0.0),
    );
    let mut i = 0;
    while i + 32 <= n {
        s0 = fmlal(s0, pa.add(i), pb.add(i));
        s1 = fmlal(s1, pa.add(i + 8), pb.add(i + 8));
        s2 = fmlal(s2, pa.add(i + 16), pb.add(i + 16));
        s3 = fmlal(s3, pa.add(i + 24), pb.add(i + 24));
        i += 32;
    }
    total(s0) + total(s1) + total(s2) + total(s3)
}

#[inline]
#[target_feature(enable = "bf16")]
unsafe fn bfdot(mut s: float32x4_t, a: *const bf16, b: *const bf16) -> float32x4_t {
    let (x, y) = (vld1q_u16(a.cast()), vld1q_u16(b.cast()));
    asm!("bfdot {0:v}.4s, {1:v}.8h, {2:v}.8h", inout(vreg) s, in(vreg) x, in(vreg) y,
        options(pure, nomem, nostack));
    s
}

#[target_feature(enable = "bf16")]
#[inline(never)]
unsafe fn bf16_fast_one(a: &[bf16], b: &[bf16]) -> f64 {
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let mut s = vdupq_n_f32(0.0);
    let mut i = 0;
    while i + 8 <= n {
        s = bfdot(s, pa.add(i), pb.add(i));
        i += 8;
    }
    total(s)
}

#[target_feature(enable = "bf16")]
#[inline(never)]
unsafe fn bf16_fast_four(a: &[bf16], b: &[bf16]) -> f64 {
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let (mut s0, mut s1, mut s2, mut s3) = (
        vdupq_n_f32(0.0),
        vdupq_n_f32(0.0),
        vdupq_n_f32(0.0),
        vdupq_n_f32(0.0),
    );
    let mut i = 0;
    while i + 32 <= n {
        s0 = bfdot(s0, pa.add(i), pb.add(i));
        s1 = bfdot(s1, pa.add(i + 8), pb.add(i + 8));
        s2 = bfdot(s2, pa.add(i + 16), pb.add(i + 16));
        s3 = bfdot(s3, pa.add(i + 24), pb.add(i + 24));
        i += 32;
    }
    total(s0) + total(s1) + total(s2) + total(s3)
}

/// Four lanes of bf16 as `f32`, a NaN's quiet bit set: `half`'s conversion on a vector.
#[inline]
unsafe fn widen(x: uint16x4_t) -> float32x4_t {
    let w = vshll_n_u16::<16>(x);
    let nan = vcgtq_u32(vandq_u32(w, vdupq_n_u32(0x7fff_ffff)), vdupq_n_u32(0x7f80_0000));
    let quiet = vorrq_u32(w, vandq_u32(nan, vdupq_n_u32(0x0040_0000)));
    vreinterpretq_f32_u32(quiet)
}

#[inline]
unsafe fn exact(s: float32x4_t, a: *const bf16, b: *const bf16) -> float32x4_t {
    let (x, y) = (vld1q_u16(a.cast()), vld1q_u16(b.cast()));
    let low = vfmaq_f32(s, widen(vget_low_u16(x)), widen(vget_low_u16(y)));
    vfmaq_f32(low, widen(vget_high_u16(x)), widen(vget_high_u16(y)))
}

#[inline(never)]
unsafe fn bf16_one(a: &[bf16], b: &[bf16]) -> f64 {
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let mut s = vdupq_n_f32(0.0);
    let mut i = 0;
    while i + 8 <= n {
        s = exact(s, pa.add(i), pb.add(i));
        i += 8;
    }
    total(s)
}

#[inline(never)]
unsafe fn bf16_four(a: &[bf16], b: &[bf16]) -> f64 {
    let n = a.len().min(b.len());
    let (pa, pb) = (a.as_ptr(), b.as_ptr());
    let (mut s0, mut s1, mut s2, mut s3) = (
        vdupq_n_f32(0.0),
        vdupq_n_f32(0.0),
        vdupq_n_f32(0.0),
        vdupq_n_f32(0.0),
    );
    let mut i = 0;
    while i + 32 <= n {
        s0 = exact(s0, pa.add(i), pb.add(i));
        s1 = exact(s1, pa.add(i + 8), pb.add(i + 8));
        s2 = exact(s2, pa.add(i + 16), pb.add(i + 16));
        s3 = exact(s3, pa.add(i + 24), pb.add(i + 24));
        i += 32;
    }
    total(s0) + total(s1) + total(s2) + total(s3)
}

#[inline(never)]
fn dot_i8(a: &[i8], b: &[i8]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i32).wrapping_mul(*y as i32))
        .fold(0i32, i32::wrapping_add)
}

fn run(passes: usize, mut kernel: impl FnMut() -> f64) {
    let mut sink = 0.0f64;
    for _ in 0..passes {
        sink += std::hint::black_box(kernel());
    }
    println!("{sink}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let kernel = args[1].as_str();
    let passes: usize = args[2].parse().unwrap();
    let n = 1_000_000usize;
    let mut r = Rng(2463534242);
    if kernel.starts_with("i8") {
        let mut w = || (0..n).map(|_| (r.next() & 255) as u8 as i8).collect::<Vec<_>>();
        let (a, b) = (w(), w());
        let mut sink = 0i32;
        for _ in 0..passes {
            sink ^= std::hint::black_box(dot_i8(std::hint::black_box(&a), &b));
        }
        println!("{sink}");
    } else if kernel.starts_with("f16") {
        let mut w = || {
            (0..n)
                .map(|_| f16::from_bits(((r.next() & 0x87ff) + 0x2c00) as u16))
                .collect::<Vec<_>>()
        };
        let (a, b) = (w(), w());
        unsafe {
            match kernel {
                "f16_one" => run(passes, || f16_one(std::hint::black_box(&a), &b)),
                // `f16_bulk` is the same four-accumulator loop, plus the tail (none at 1M).
                _ => run(passes, || f16_four(std::hint::black_box(&a), &b)),
            }
        }
    } else {
        let mut w = || {
            (0..n)
                .map(|_| bf16::from_bits(((r.next() & 0x80ff) + 0x3b00) as u16))
                .collect::<Vec<_>>()
        };
        let (a, b) = (w(), w());
        unsafe {
            match kernel {
                "bf16_one" => run(passes, || bf16_one(std::hint::black_box(&a), &b)),
                "bf16_fast_one" => run(passes, || bf16_fast_one(std::hint::black_box(&a), &b)),
                "bf16_fast_four" => run(passes, || bf16_fast_four(std::hint::black_box(&a), &b)),
                _ => run(passes, || bf16_four(std::hint::black_box(&a), &b)),
            }
        }
    }
}
