// The Rust side of kernels.vlt: the same plain loops, and the SIMD versions written the way
// stable Rust offers, with the target's intrinsics (`core::arch`: NEON on aarch64, SSE2 on
// x86-64), since `std::simd` is nightly-only.
use std::hint::black_box;
use std::time::Instant;

const N: usize = 16384;
const REPS: i64 = 100000;

#[cfg(target_arch = "aarch64")]
mod v {
    use std::arch::aarch64::*;
    pub type U8 = uint8x16_t;
    pub type F32 = float32x4_t;
    pub unsafe fn load(p: *const u8) -> U8 { vld1q_u8(p) }
    pub unsafe fn store(p: *mut u8, x: U8) { vst1q_u8(p, x) }
    pub unsafe fn splat(b: u8) -> U8 { vdupq_n_u8(b) }
    pub unsafe fn eq(a: U8, b: U8) -> U8 { vceqq_u8(a, b) }
    pub unsafe fn ge(a: U8, b: U8) -> U8 { vcgeq_u8(a, b) }
    pub unsafe fn le(a: U8, b: U8) -> U8 { vcleq_u8(a, b) }
    pub unsafe fn and(a: U8, b: U8) -> U8 { vandq_u8(a, b) }
    pub unsafe fn or(a: U8, b: U8) -> U8 { vorrq_u8(a, b) }
    pub unsafe fn xor(a: U8, b: U8) -> U8 { veorq_u8(a, b) }
    pub unsafe fn add(a: U8, b: U8) -> U8 { vaddq_u8(a, b) }
    pub unsafe fn select(m: U8, a: U8, b: U8) -> U8 { vbslq_u8(m, a, b) }
    pub unsafe fn any(m: U8) -> bool { vmaxvq_u8(m) != 0 }
    /// Four bits per lane (the `shrn` narrowing memchr uses).
    unsafe fn nibbles(m: U8) -> u64 {
        vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(m))))
    }
    pub unsafe fn first(m: U8) -> usize { (nibbles(m).trailing_zeros() >> 2) as usize }
    pub unsafe fn count(m: U8) -> u64 { u64::from(vaddvq_u8(vshrq_n_u8::<7>(m))) }
    pub unsafe fn fzero() -> F32 { vdupq_n_f32(0.0) }
    pub unsafe fn fload(p: *const f32) -> F32 { vld1q_f32(p) }
    pub unsafe fn fadd(a: F32, b: F32) -> F32 { vaddq_f32(a, b) }
    pub unsafe fn fmul(a: F32, b: F32) -> F32 { vmulq_f32(a, b) }
    pub unsafe fn fsum(a: F32) -> f32 {
        vgetq_lane_f32::<0>(a) + vgetq_lane_f32::<1>(a) + vgetq_lane_f32::<2>(a) + vgetq_lane_f32::<3>(a)
    }
}

#[cfg(target_arch = "x86_64")]
mod v {
    use std::arch::x86_64::*;
    pub type U8 = __m128i;
    pub type F32 = __m128;
    pub unsafe fn load(p: *const u8) -> U8 { _mm_loadu_si128(p.cast()) }
    pub unsafe fn store(p: *mut u8, x: U8) { _mm_storeu_si128(p.cast(), x) }
    pub unsafe fn splat(b: u8) -> U8 { _mm_set1_epi8(b as i8) }
    pub unsafe fn eq(a: U8, b: U8) -> U8 { _mm_cmpeq_epi8(a, b) }
    pub unsafe fn ge(a: U8, b: U8) -> U8 { _mm_cmpeq_epi8(_mm_max_epu8(a, b), a) }
    pub unsafe fn le(a: U8, b: U8) -> U8 { _mm_cmpeq_epi8(_mm_min_epu8(a, b), a) }
    pub unsafe fn and(a: U8, b: U8) -> U8 { _mm_and_si128(a, b) }
    pub unsafe fn or(a: U8, b: U8) -> U8 { _mm_or_si128(a, b) }
    pub unsafe fn xor(a: U8, b: U8) -> U8 { _mm_xor_si128(a, b) }
    pub unsafe fn add(a: U8, b: U8) -> U8 { _mm_add_epi8(a, b) }
    pub unsafe fn select(m: U8, a: U8, b: U8) -> U8 {
        _mm_or_si128(_mm_and_si128(m, a), _mm_andnot_si128(m, b))
    }
    pub unsafe fn any(m: U8) -> bool { _mm_movemask_epi8(m) != 0 }
    pub unsafe fn first(m: U8) -> usize { _mm_movemask_epi8(m).trailing_zeros() as usize }
    pub unsafe fn count(m: U8) -> u64 { u64::from(_mm_movemask_epi8(m).count_ones()) }
    pub unsafe fn fzero() -> F32 { _mm_setzero_ps() }
    pub unsafe fn fload(p: *const f32) -> F32 { _mm_loadu_ps(p) }
    pub unsafe fn fadd(a: F32, b: F32) -> F32 { _mm_add_ps(a, b) }
    pub unsafe fn fmul(a: F32, b: F32) -> F32 { _mm_mul_ps(a, b) }
    pub unsafe fn fsum(a: F32) -> f32 {
        let mut l = [0f32; 4];
        _mm_storeu_ps(l.as_mut_ptr(), a);
        l[0] + l[1] + l[2] + l[3]
    }
}

#[inline(never)]
fn sum_bytes(d: &[u8]) -> u64 {
    let mut s = 0u64;
    for &x in d {
        s += u64::from(x);
    }
    s
}

#[inline(never)]
fn count_loop(d: &[u8], b: u8) -> u64 {
    let mut c = 0u64;
    for &x in d {
        if x == b {
            c += 1;
        }
    }
    c
}

#[inline(never)]
fn count_simd(d: &[u8], b: u8) -> u64 {
    let (mut c, mut i) = (0u64, 0);
    unsafe {
        let needle = v::splat(b);
        while i + 16 <= d.len() {
            c += v::count(v::eq(v::load(d.as_ptr().add(i)), needle));
            i += 16;
        }
    }
    c + count_loop(&d[i..], b)
}

#[inline(never)]
fn find_loop(d: &[u8], b: u8) -> i64 {
    for i in 0..d.len() {
        if d[i] == b {
            return i as i64;
        }
    }
    -1
}

#[inline(never)]
fn find_simd(d: &[u8], b: u8) -> i64 {
    let mut i = 0;
    unsafe {
        let needle = v::splat(b);
        while i + 16 <= d.len() {
            let m = v::eq(v::load(d.as_ptr().add(i)), needle);
            if v::any(m) {
                return (i + v::first(m)) as i64;
            }
            i += 16;
        }
    }
    match find_loop(&d[i..], b) {
        -1 => -1,
        at => i as i64 + at,
    }
}

#[inline(never)]
fn ascii_loop(d: &[u8]) -> bool {
    let mut a = 0u8;
    for &x in d {
        a |= x;
    }
    a < 128
}

#[inline(never)]
fn ascii_simd(d: &[u8]) -> bool {
    let mut i = 0;
    let high = unsafe {
        let mut acc = [v::splat(0); 4];
        while i + 64 <= d.len() {
            for (k, a) in acc.iter_mut().enumerate() {
                *a = v::or(*a, v::load(d.as_ptr().add(i + 16 * k)));
            }
            i += 64;
        }
        let all = v::or(v::or(acc[0], acc[1]), v::or(acc[2], acc[3]));
        v::any(v::ge(all, v::splat(128)))
    };
    !high && ascii_loop(&d[i..])
}

#[inline(never)]
fn xor_loop(d: &mut [u8], key: &[u8; 4]) {
    for i in 0..d.len() {
        d[i] ^= key[i & 3];
    }
}

#[inline(never)]
fn xor_simd(d: &mut [u8], key: &[u8; 4]) {
    let mut repeated = [0u8; 16];
    for (j, r) in repeated.iter_mut().enumerate() {
        *r = key[j & 3];
    }
    let mut i = 0;
    unsafe {
        let k = v::load(repeated.as_ptr());
        while i + 16 <= d.len() {
            let p = d.as_mut_ptr().add(i);
            v::store(p, v::xor(v::load(p), k));
            i += 16;
        }
    }
    while i < d.len() {
        d[i] ^= key[i & 3];
        i += 1;
    }
}

#[inline(never)]
fn lower_loop(d: &mut [u8]) {
    for x in d.iter_mut() {
        if *x >= 65 && *x <= 90 {
            *x += 32;
        }
    }
}

#[inline(never)]
fn lower_simd(d: &mut [u8]) {
    let mut i = 0;
    unsafe {
        while i + 16 <= d.len() {
            let p = d.as_mut_ptr().add(i);
            let x = v::load(p);
            let upper = v::and(v::ge(x, v::splat(65)), v::le(x, v::splat(90)));
            v::store(p, v::select(upper, v::add(x, v::splat(32)), x));
            i += 16;
        }
    }
    lower_loop(&mut d[i..]);
}

#[inline(never)]
fn dot_loop(a: &[f32], b: &[f32]) -> f32 {
    let mut s = 0f32;
    for i in 0..a.len() {
        s += a[i] * b[i];
    }
    s
}

#[inline(never)]
fn dot_simd(a: &[f32], b: &[f32]) -> f32 {
    let mut i = 0;
    let mut s = unsafe {
        let mut acc = [v::fzero(); 4];
        while i + 16 <= a.len() {
            for (k, x) in acc.iter_mut().enumerate() {
                let (pa, pb) = (a.as_ptr().add(i + 4 * k), b.as_ptr().add(i + 4 * k));
                *x = v::fadd(*x, v::fmul(v::fload(pa), v::fload(pb)));
            }
            i += 16;
        }
        v::fsum(v::fadd(v::fadd(acc[0], acc[1]), v::fadd(acc[2], acc[3])))
    };
    while i < a.len() {
        s += a[i] * b[i];
        i += 1;
    }
    s
}

fn text() -> Vec<u8> {
    let mut d: Vec<u8> = (0..N)
        .map(|i| ((if i % 3 == 0 { 65 } else { 97 }) + (i * 7) % 26) as u8)
        .collect();
    d[N - 1] = 33;
    d
}

fn floats(k: usize) -> Vec<f32> {
    (0..4096).map(|i| (i % k) as f32).collect()
}

fn report(name: &str, t0: Instant, check: u64) {
    println!("{name} {} {check}", t0.elapsed().as_millis());
}

fn touch(d: &mut [u8], r: i64) {
    d[r as usize % (N - 1)] = (97 + r % 26) as u8;
}

fn bytes(name: &str, kernel: impl Fn(&[u8]) -> u64) {
    let mut d = text();
    let t0 = Instant::now();
    let mut acc = 0u64;
    for r in 0..REPS {
        touch(&mut d, r);
        acc = acc.wrapping_add(kernel(black_box(&d)));
    }
    report(name, t0, acc);
}

fn dots(name: &str, kernel: impl Fn(&[f32], &[f32]) -> f32) {
    let (mut fa, fb) = (floats(7), floats(5));
    let t0 = Instant::now();
    let mut fs = 0f32;
    for r in 0..REPS * 4 {
        fa[r as usize % 4096] = (r % 3) as f32;
        fs += kernel(black_box(&fa), &fb);
    }
    report(name, t0, fs as u64);
}

fn main() {
    let key = [0x12u8, 0x34, 0x56, 0x78];
    bytes("sum_loop", sum_bytes);
    bytes("count_loop", |d| count_loop(d, 101));
    bytes("count_simd", |d| count_simd(d, 101));
    bytes("find_loop", |d| find_loop(d, 33) as u64);
    bytes("find_simd", |d| find_simd(d, 33) as u64);
    bytes("ascii_loop", |d| u64::from(ascii_loop(d)));
    bytes("ascii_simd", |d| u64::from(ascii_simd(d)));
    for (name, kernel) in [("xor_loop", xor_loop as fn(&mut [u8], &[u8; 4])), ("xor_simd", xor_simd)] {
        let mut d = text();
        let t0 = Instant::now();
        for _ in 0..REPS + 1 {
            kernel(black_box(&mut d), &key);
        }
        report(name, t0, sum_bytes(&d));
    }
    for (name, kernel) in [("lower_loop", lower_loop as fn(&mut [u8])), ("lower_simd", lower_simd)] {
        let mut d = text();
        let t0 = Instant::now();
        for r in 0..REPS {
            d[r as usize % (N - 1)] = (65 + r % 26) as u8;
            kernel(black_box(&mut d));
        }
        report(name, t0, sum_bytes(&d));
    }
    dots("dot_loop", dot_loop);
    dots("dot_simd", dot_simd);
}
