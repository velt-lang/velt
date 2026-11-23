// The runtime's `velt_rt_pow_i64` (velt_rt `math.rs` before #2196), called through a function
// the optimizer may not inline, as compiled Velt code called it.
#[inline(never)]
fn pow_i64(a: i64, b: i64) -> i64 {
    if b < 0 {
        return if a == 1 { 1 } else { 0 };
    }
    let (mut base, mut exp, mut acc) = (a, b as u64, 1i64);
    while exp > 0 {
        if exp & 1 == 1 {
            acc = acc.wrapping_mul(base);
        }
        exp >>= 1;
        if exp > 0 {
            base = base.wrapping_mul(base);
        }
    }
    acc
}

fn main() {
    let mut sum = 0i64;
    for i in 0..20_000_000i64 {
        let base = (i % 100) - 50;
        let exp = (i * 7) % 64;
        sum = sum.wrapping_add(pow_i64(std::hint::black_box(base), exp));
    }
    println!("{sum}");
}
