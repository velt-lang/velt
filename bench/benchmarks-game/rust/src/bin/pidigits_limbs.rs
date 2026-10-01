// pidigits without GMP: a line-by-line transliteration of bench/benchmarks-game/pidigits/main.vlt
// (the Node #2 spigot by Isaac Gouy after Alexander Fyodorov, on a minimal sign + base-2^32 limb
// integer). Not a Benchmarks Game program: it separates "Velt codegen vs Rust codegen" (this vs
// the Velt port) from "hand-written limb loops vs GMP" (this vs pidigits.rs).

const LIMB_BASE: f64 = 4294967296.0;

struct BigInt {
    negative: bool,
    limbs: Vec<u32>,
}

impl BigInt {
    fn new(value: u32) -> BigInt {
        let mut limbs = Vec::new();
        if value != 0 {
            limbs.push(value);
        }
        BigInt { negative: false, limbs }
    }

    fn assign(&mut self, other: &BigInt) {
        self.negative = other.negative;
        self.limbs.clear();
        self.limbs.extend_from_slice(&other.limbs);
    }

    fn mul_small(&mut self, k: u64) {
        let mut carry = 0u64;
        for limb in self.limbs.iter_mut() {
            let p = *limb as u64 * k + carry;
            *limb = p as u32;
            carry = p >> 32;
        }
        if carry != 0 {
            self.limbs.push(carry as u32);
        }
    }

    fn add_mul(&mut self, other: &BigInt, k: u64) {
        if self.negative {
            self.sub_magnitude(other, k);
        } else {
            self.add_magnitude(other, k);
        }
    }

    fn sub_mul(&mut self, other: &BigInt, k: u64) {
        if self.negative {
            self.add_magnitude(other, k);
        } else {
            self.sub_magnitude(other, k);
        }
    }

    fn compare(&self, other: &BigInt) -> i64 {
        if self.negative != other.negative {
            return if self.negative { -1 } else { 1 };
        }
        let magnitude = self.compare_magnitude(other);
        if self.negative {
            -magnitude
        } else {
            magnitude
        }
    }

    fn div_rem_small_quotient(&mut self, divisor: &BigInt) -> u64 {
        if self.compare_magnitude(divisor) < 0 {
            return 0;
        }
        let top = divisor.limbs.len() as i64 - 1;
        let estimate = self.top_value(top) / divisor.top_value(top);
        let mut quotient = estimate as u64;
        if quotient > 0 && estimate - (quotient as f64) < 0.000001 {
            quotient -= 1;
        }
        self.sub_magnitude(divisor, quotient);
        while self.compare_magnitude(divisor) >= 0 {
            self.sub_magnitude(divisor, 1);
            quotient += 1;
        }
        quotient
    }

    fn add_magnitude(&mut self, other: &BigInt, k: u64) {
        let mut carry = 0u64;
        let mut i = 0;
        while i < other.limbs.len() || carry != 0 {
            let o = if i < other.limbs.len() { other.limbs[i] as u64 } else { 0 };
            let p = o * k + carry;
            if i < self.limbs.len() {
                let s = self.limbs[i] as u64 + (p & 0xffff_ffff);
                self.limbs[i] = s as u32;
                carry = (p >> 32) + (s >> 32);
            } else {
                self.limbs.push(p as u32);
                carry = p >> 32;
            }
            i += 1;
        }
    }

    fn sub_magnitude(&mut self, other: &BigInt, k: u64) {
        while self.limbs.len() <= other.limbs.len() {
            self.limbs.push(0);
        }
        let mut borrow = 0u64;
        for i in 0..self.limbs.len() {
            let o = if i < other.limbs.len() { other.limbs[i] as u64 } else { 0 };
            let p = o * k + borrow;
            let low = p & 0xffff_ffff;
            let current = self.limbs[i] as u64;
            self.limbs[i] = current.wrapping_sub(low) as u32;
            borrow = (p >> 32) + (current < low) as u64;
        }
        if borrow != 0 {
            let mut carry = 1u64;
            for limb in self.limbs.iter_mut() {
                let s = (!*limb) as u64 + carry;
                *limb = s as u32;
                carry = s >> 32;
            }
            self.negative = !self.negative;
        }
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
        if self.limbs.is_empty() {
            self.negative = false;
        }
    }

    fn compare_magnitude(&self, other: &BigInt) -> i64 {
        if self.limbs.len() != other.limbs.len() {
            return if self.limbs.len() < other.limbs.len() { -1 } else { 1 };
        }
        for i in (0..self.limbs.len()).rev() {
            let (a, b) = (self.limbs[i], other.limbs[i]);
            if a != b {
                return if a < b { -1 } else { 1 };
            }
        }
        0
    }

    fn top_value(&self, top: i64) -> f64 {
        let mut value = 0.0;
        let mut i = top + 1;
        while i >= top - 1 {
            value *= LIMB_BASE;
            if i >= 0 && i < self.limbs.len() as i64 {
                value += self.limbs[i as usize] as f64;
            }
            i -= 1;
        }
        value
    }
}

fn main() {
    let n: i64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(27);
    let mut acc = BigInt::new(0);
    let mut den = BigInt::new(1);
    let mut num = BigInt::new(1);
    let mut tmp = BigInt::new(0);
    let mut k = 0u64;
    let mut line = String::new();
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        k += 1;
        let k2 = k * 2 + 1;
        acc.add_mul(&num, 2);
        acc.mul_small(k2);
        den.mul_small(k2);
        num.mul_small(k);
        if num.compare(&acc) > 0 {
            continue;
        }
        tmp.assign(&acc);
        tmp.add_mul(&num, 3);
        let digit = tmp.div_rem_small_quotient(&den);
        tmp.add_mul(&num, 1);
        if tmp.compare(&den) >= 0 {
            continue;
        }
        line.push_str(&digit.to_string());
        i += 1;
        if i % 10 == 0 || i == n {
            out.push_str(&format!("{:<10}\t:{}\n", line, i));
            line.clear();
        }
        acc.sub_mul(&den, digit);
        acc.mul_small(10);
        num.mul_small(10);
    }
    print!("{}", out);
}
