//! Prime-field arithmetic that backends emit inline (`velt_field_*`, rt_abi.md "Field
//! arithmetic"): the Montgomery multiplication and squaring of std's elliptic-curve fields
//! (std/crypto/field_p256.velt), which dominate a signature check. Each takes the field's
//! register array (`FeP256.r`, a pointer to its header) and register numbers: a register is four
//! consecutive `u64` limbs, little-endian, at element `4 * n` of the array's data. The operation
//! reads its operand registers whole before it writes its result register, which may be one of
//! them. It does no bounds check (std checks the registers once), keeps no pointer, cannot unwind
//! and touches no other memory.
//!
//! [`FieldOp::eval`] is the reference every backend matches: LLVM emits it as a scheduled
//! sequence of the target's multiply-with-carry instructions, Cranelift as the plain sequence.

use crate::vir::Ty;

/// A field operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldOp {
    /// `(r, d, a, b)`: register `d` = `a * b / 2^256` mod p256, fully reduced (below p).
    P256Mul,
    /// `(r, d, a)`: register `d` = `a * a / 2^256` mod p256, fully reduced.
    P256Sqr,
    /// `(r, d, a, b)`: register `d` = `a + b` mod p256 (both below p).
    P256Add,
    /// `(r, d, a, b)`: register `d` = `a - b` mod p256 (both below p).
    P256Sub,
    /// `(r, d, p) -> bool`: the Jacobian point at registers `d..d+3` = twice the one at `p..p+3`
    /// on y^2 = x^3 - 3x + b ([`double_p256`]), `d` may be `p`; true when done, false where the
    /// target has no fused form (the caller doubles with the other operations then).
    P256Double,
}

/// The P-256 prime, little-endian limbs.
pub const P256: [u64; 4] = [
    0xffff_ffff_ffff_ffff,
    0x0000_0000_ffff_ffff,
    0,
    0xffff_ffff_0000_0001,
];

impl FieldOp {
    pub const ALL: [FieldOp; 5] = [
        FieldOp::P256Mul,
        FieldOp::P256Sqr,
        FieldOp::P256Add,
        FieldOp::P256Sub,
        FieldOp::P256Double,
    ];

    /// The extern's symbol.
    pub const fn symbol(self) -> &'static str {
        match self {
            FieldOp::P256Mul => "velt_field_p256_mul",
            FieldOp::P256Sqr => "velt_field_p256_sqr",
            FieldOp::P256Add => "velt_field_p256_add",
            FieldOp::P256Sub => "velt_field_p256_sub",
            FieldOp::P256Double => "velt_field_p256_double",
        }
    }

    /// The operation `symbol` names, if it names one.
    pub fn of_symbol(symbol: &str) -> Option<FieldOp> {
        FieldOp::ALL.into_iter().find(|op| op.symbol() == symbol)
    }

    /// How many registers it reads (`a`, or `a` and `b`).
    pub const fn operands(self) -> usize {
        match self {
            FieldOp::P256Sqr | FieldOp::P256Double => 1,
            FieldOp::P256Mul | FieldOp::P256Add | FieldOp::P256Sub => 2,
        }
    }

    /// The VIR signature: the array, the result's register, then the operands'.
    pub fn signature(self) -> (Vec<Ty>, Ty) {
        let mut params = vec![Ty::Ptr, Ty::U64];
        params.extend(std::iter::repeat_n(Ty::U64, self.operands()));
        let ret = match self {
            FieldOp::P256Double => Ty::Bool,
            _ => Ty::Unit,
        };
        (params, ret)
    }

    /// The result for operand registers `a` and `b` (`b` ignored for a square), each fully
    /// reduced: the reference.
    pub fn eval(self, a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
        match self {
            FieldOp::P256Mul => mont_mul(a, b, P256),
            FieldOp::P256Sqr => mont_mul(a, a, P256),
            FieldOp::P256Add => add_mod(a, b, P256),
            FieldOp::P256Sub => sub_mod(a, b, P256),
            FieldOp::P256Double => panic!("ICE: a point doubling is `double_p256`"),
        }
    }
}

/// Twice the Jacobian point `(x, y, z)` (Montgomery form) on P-256: dbl-2001-b for a = -3, as
/// std/crypto/p256.velt `double` computes it.
pub fn double_p256(x: [u64; 4], y: [u64; 4], z: [u64; 4]) -> [[u64; 4]; 3] {
    let (mul, add, sub) = (
        |a, b| mont_mul(a, b, P256),
        |a, b| add_mod(a, b, P256),
        |a, b| sub_mod(a, b, P256),
    );
    let delta = mul(z, z);
    let gamma = mul(y, y);
    let beta = mul(x, gamma);
    let alpha = mul(sub(x, delta), add(x, delta));
    let alpha = add(add(alpha, alpha), alpha);
    let yz = add(y, z);
    let z3 = sub(sub(mul(yz, yz), gamma), delta);
    let beta4 = add(add(beta, beta), add(beta, beta));
    let x3 = sub(mul(alpha, alpha), add(beta4, beta4));
    let g2 = mul(gamma, gamma);
    let g8 = add(add(add(g2, g2), add(g2, g2)), add(add(g2, g2), add(g2, g2)));
    let y3 = sub(mul(alpha, sub(beta4, x3)), g8);
    [x3, y3, z3]
}

/// `a + b` mod `p` for `a`, `b` below `p`.
fn add_mod(a: [u64; 4], b: [u64; 4], p: [u64; 4]) -> [u64; 4] {
    let (mut s, mut carry) = ([0u64; 4], 0u128);
    for j in 0..4 {
        let t = u128::from(a[j]) + u128::from(b[j]) + carry;
        s[j] = t as u64;
        carry = t >> 64;
    }
    let (mut d, mut borrow) = ([0u64; 4], 0u64);
    for j in 0..4 {
        let (x, b1) = s[j].overflowing_sub(p[j]);
        let (y, b2) = x.overflowing_sub(borrow);
        d[j] = y;
        borrow = u64::from(b1 || b2);
    }
    if carry != 0 || borrow == 0 {
        d
    } else {
        s
    }
}

/// `a - b` mod `p` for `a`, `b` below `p`.
fn sub_mod(a: [u64; 4], b: [u64; 4], p: [u64; 4]) -> [u64; 4] {
    let (mut d, mut borrow) = ([0u64; 4], 0u64);
    for j in 0..4 {
        let (x, b1) = a[j].overflowing_sub(b[j]);
        let (y, b2) = x.overflowing_sub(borrow);
        d[j] = y;
        borrow = u64::from(b1 || b2);
    }
    if borrow == 0 {
        return d;
    }
    let mut carry = 0u128;
    for j in 0..4 {
        let t = u128::from(d[j]) + u128::from(p[j]) + carry;
        d[j] = t as u64;
        carry = t >> 64;
    }
    d
}

/// `a * b / 2^256` mod `p` for a 256-bit odd `p` with `p[0] = 2^64 - 1` (so `-1/p` mod 2^64 is
/// 1), `a` and `b` below `p`: the result is below `p`.
fn mont_mul(a: [u64; 4], b: [u64; 4], p: [u64; 4]) -> [u64; 4] {
    // t = a * b, 512 bits.
    let mut t = [0u64; 9];
    for (i, &ai) in a.iter().enumerate() {
        let mut carry = 0u128;
        for (j, &bj) in b.iter().enumerate() {
            let s = u128::from(ai) * u128::from(bj) + u128::from(t[i + j]) + carry;
            t[i + j] = s as u64;
            carry = s >> 64;
        }
        t[i + 4] = carry as u64;
    }
    // Four rounds: t += m p with m = t[i] (p = -1 mod 2^64), which clears limb i.
    for i in 0..4 {
        let m = t[i];
        let mut carry = 0u128;
        for (j, &pj) in p.iter().enumerate() {
            let s = u128::from(m) * u128::from(pj) + u128::from(t[i + j]) + carry;
            t[i + j] = s as u64;
            carry = s >> 64;
        }
        for limb in &mut t[i + 4..] {
            let s = u128::from(*limb) + carry;
            *limb = s as u64;
            carry = s >> 64;
        }
    }
    // t[4..9] is below 2p: subtract p once if it is not below p.
    let r = [t[4], t[5], t[6], t[7]];
    let mut d = [0u64; 4];
    let mut borrow = 0u64;
    for j in 0..4 {
        let (x, b1) = r[j].overflowing_sub(p[j]);
        let (y, b2) = x.overflowing_sub(borrow);
        d[j] = y;
        borrow = u64::from(b1 || b2);
    }
    if t[8] != 0 || borrow == 0 {
        d
    } else {
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R = 2^256 mod p, the Montgomery form of 1; R^2 mod p enters the domain.
    const ONE_M: [u64; 4] = [1, 0xffff_ffff_0000_0000, 0xffff_ffff_ffff_ffff, 0xffff_fffe];
    const R2: [u64; 4] = [
        3,
        0xffff_fffb_ffff_ffff,
        0xffff_ffff_ffff_fffe,
        0x4_ffff_fffd,
    ];

    #[test]
    fn one_and_r_squared() {
        // 1 * R^2 / R = R; R * R / R = R; (p - 1)^2 / R reduces fully.
        assert_eq!(FieldOp::P256Mul.eval([1, 0, 0, 0], R2), ONE_M);
        assert_eq!(FieldOp::P256Sqr.eval(ONE_M, [0; 4]), ONE_M);
        let pm1 = [P256[0] - 1, P256[1], P256[2], P256[3]];
        let r = FieldOp::P256Sqr.eval(pm1, [0; 4]);
        // (-1)^2 = 1, so the result is 1 / R, which times R^2 / R is 1.
        assert_eq!(FieldOp::P256Mul.eval(r, R2), [1, 0, 0, 0]);
    }

    #[test]
    fn add_and_sub() {
        let pm1 = [P256[0] - 1, P256[1], P256[2], P256[3]];
        assert_eq!(FieldOp::P256Add.eval(pm1, [1, 0, 0, 0]), [0; 4]);
        assert_eq!(FieldOp::P256Add.eval(pm1, [2, 0, 0, 0]), [1, 0, 0, 0]);
        assert_eq!(FieldOp::P256Sub.eval([0; 4], [1, 0, 0, 0]), pm1);
        assert_eq!(
            FieldOp::P256Sub.eval([5, 0, 0, 0], [3, 0, 0, 0]),
            [2, 0, 0, 0]
        );
    }

    #[test]
    fn signatures() {
        assert_eq!(
            FieldOp::P256Mul.signature(),
            (vec![Ty::Ptr, Ty::U64, Ty::U64, Ty::U64], Ty::Unit)
        );
        assert_eq!(
            FieldOp::of_symbol("velt_field_p256_sqr"),
            Some(FieldOp::P256Sqr)
        );
    }
}
