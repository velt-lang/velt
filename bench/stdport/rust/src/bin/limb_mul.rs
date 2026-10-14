fn main() {
    let mut limbs: Vec<u64> = Vec::new();
    let mut x: u64 = 11400714819323198485;
    for _ in 0..64 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        limbs.push(x);
    }
    let mut acc: u64 = 0;
    for round in 1..=2_000_000u64 {
        let k = x.wrapping_add(round);
        let mut carry: u64 = 0;
        for &limb in &limbs {
            let wide = limb as u128 * k as u128;
            let lo = wide as u64;
            let sum = lo.wrapping_add(carry);
            carry = ((wide >> 64) as u64).wrapping_add((sum < lo) as u64);
            acc ^= sum;
        }
        acc ^= carry;
    }
    println!("{acc}");
}
