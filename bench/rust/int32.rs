// 32-bit integer mixing (same as int32.vlt). `mul_js` is JavaScript's `(a * b) | 0` on int32
// numbers: the exact product (it fits an i64) rounded to a double like JS's multiply, then its low
// 32 bits. `mix32imul` is `Math.imul`, Rust's `wrapping_mul`.
fn mul_js(a: i32, b: i32) -> i32 {
    let p = a as i64 * b as i64;
    (p as f64) as i64 as i32
}

fn mix32(x: i32, i: i64) -> i32 {
    let mut y = x ^ ((x as u32) >> 15) as i32;
    y = mul_js(y, 0x2c1b3c6d);
    y = (y as i64 + i) as i32;
    y ^= ((y as u32) >> 12) as i32;
    y = mul_js(y, 0x297a2d39);
    y ^= ((y as u32) >> 15) as i32;
    y
}

fn mix32imul(x: i32, i: i64) -> i32 {
    let mut y = x ^ ((x as u32) >> 15) as i32;
    y = y.wrapping_mul(0x2c1b3c6d);
    y = (y as i64 + i) as i32;
    y ^= ((y as u32) >> 12) as i32;
    y = y.wrapping_mul(0x297a2d39);
    y ^= ((y as u32) >> 15) as i32;
    y
}

fn run(iterations: i64, imul: bool) -> i32 {
    let mut checksum = 0i32;
    for sample in 0..2 {
        let mut state = 0x12345678i32.wrapping_add(sample);
        let mut acc = 0i32;
        for i in 0..iterations {
            state = if imul { mix32imul(state, i) } else { mix32(state, i) };
            acc = acc.wrapping_add(state);
        }
        checksum = checksum.wrapping_add(acc);
    }
    checksum
}

fn main() {
    println!("{}", run(50_000_000, false));
    println!("{}", run(50_000_000, true));
}
