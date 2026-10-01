// Integer loops: Collatz chain lengths and a nested multiply/modulo loop (same as loops.vlt).
fn collatz_steps(start: i64) -> i64 {
    let mut n = start;
    let mut steps = 0i64;
    while n != 1 {
        if n % 2 == 0 {
            n /= 2;
        } else {
            n = n.wrapping_mul(3).wrapping_add(1);
        }
        steps += 1;
    }
    steps
}

fn main() {
    let mut best = 0i64;
    let mut best_start = 0i64;
    for i in 1..1_000_000i64 {
        let s = collatz_steps(i);
        if s > best {
            best = s;
            best_start = i;
        }
    }
    println!("{best_start} {best}");

    let mut sum = 0i64;
    for i in 0..4000i64 {
        for j in 0..4000i64 {
            sum = sum.wrapping_add(i.wrapping_mul(j)).wrapping_add(i + j) % 1_000_000_007;
        }
    }
    println!("{sum}");
}
