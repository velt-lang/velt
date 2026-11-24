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
    /// `(r, d, p) -> bool`: the Jacobian point at registers `d`, `d + 1`, `d + 2` (X, Y, Z) =
    /// twice the one at `p`, `p + 1`, `p + 2` on y^2 = x^3 - 3x + b ([`double_p256`]), `d` may be
    /// `p`; true when done, false where the target has no fused form (the caller doubles with
    /// the other operations then).
    P256Double,
    /// `(r, d, u, k) -> bool`: registers `d` and `d + 1` = X2 and Z2 (fully reduced) after RFC
    /// 7748's Montgomery ladder on Curve25519 for the u-coordinate at register `u` (four 64-bit
    /// limbs, below 2^256, not necessarily reduced) and the scalar at register `k` (already
    /// clamped), [`x25519_ladder`]; true when done, false where the target has no fused form
    /// (std runs its own ladder then).
    X25519Ladder,
}

/// The P-256 prime, little-endian limbs.
pub const P256: [u64; 4] = [
    0xffff_ffff_ffff_ffff,
    0x0000_0000_ffff_ffff,
    0,
    0xffff_ffff_0000_0001,
];

impl FieldOp {
    pub const ALL: [FieldOp; 6] = [
        FieldOp::P256Mul,
        FieldOp::P256Sqr,
        FieldOp::P256Add,
        FieldOp::P256Sub,
        FieldOp::P256Double,
        FieldOp::X25519Ladder,
    ];

    /// The extern's symbol.
    pub const fn symbol(self) -> &'static str {
        match self {
            FieldOp::P256Mul => "velt_field_p256_mul",
            FieldOp::P256Sqr => "velt_field_p256_sqr",
            FieldOp::P256Add => "velt_field_p256_add",
            FieldOp::P256Sub => "velt_field_p256_sub",
            FieldOp::P256Double => "velt_field_p256_double",
            FieldOp::X25519Ladder => "velt_field_x25519_ladder",
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
            FieldOp::P256Mul | FieldOp::P256Add | FieldOp::P256Sub | FieldOp::X25519Ladder => 2,
        }
    }

    /// The VIR signature: the array, the result's register, then the operands'.
    pub fn signature(self) -> (Vec<Ty>, Ty) {
        let mut params = vec![Ty::Ptr, Ty::U64];
        params.extend(std::iter::repeat_n(Ty::U64, self.operands()));
        let ret = match self {
            FieldOp::P256Double | FieldOp::X25519Ladder => Ty::Bool,
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
            FieldOp::X25519Ladder => panic!("ICE: the X25519 ladder is `x25519_ladder`"),
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

/// p = 2^255 - 19, little-endian limbs.
pub const P25519: [u64; 4] = [
    0xffff_ffff_ffff_ffed,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x7fff_ffff_ffff_ffff,
];

/// `a` (below 2^256) reduced mod 2^255 - 19.
pub fn freeze25519(a: [u64; 4]) -> [u64; 4] {
    // 2^255 = 19: fold bit 255 in twice (the first fold can carry into it once more).
    let mut t = a;
    for _ in 0..2 {
        let top = t[3] >> 63;
        t[3] &= 0x7fff_ffff_ffff_ffff;
        let mut carry = u128::from(top * 19);
        for limb in &mut t {
            let s = u128::from(*limb) + carry;
            *limb = s as u64;
            carry = s >> 64;
        }
    }
    // Now below 2^255: less p when not below it.
    let (mut d, mut borrow) = ([0u64; 4], 0u64);
    for j in 0..4 {
        let (x, b1) = t[j].overflowing_sub(P25519[j]);
        let (y, b2) = x.overflowing_sub(borrow);
        d[j] = y;
        borrow = u64::from(b1 || b2);
    }
    if borrow == 0 {
        d
    } else {
        t
    }
}

fn add25519(a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
    add_mod(freeze25519(a), freeze25519(b), P25519)
}

fn sub25519(a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
    sub_mod(freeze25519(a), freeze25519(b), P25519)
}

fn mul25519(a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
    let mut t = [0u64; 8];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let s = u128::from(a[i]) * u128::from(b[j]) + u128::from(t[i + j]) + carry;
            t[i + j] = s as u64;
            carry = s >> 64;
        }
        t[i + 4] = carry as u64;
    }
    // t = lo + 2^256 hi = lo + 38 hi.
    let mut r = [0u64; 4];
    let mut carry = 0u128;
    for j in 0..4 {
        let s = u128::from(t[j]) + 38 * u128::from(t[4 + j]) + carry;
        r[j] = s as u64;
        carry = s >> 64;
    }
    // carry * 2^256 = 38 carry, which fits; once more for a last carry.
    let mut c = carry * 38;
    for _ in 0..2 {
        for limb in &mut r {
            let s = u128::from(*limb) + c;
            *limb = s as u64;
            c = s >> 64;
        }
        c *= 38;
    }
    freeze25519(r)
}

/// X2 and Z2, fully reduced, after RFC 7748 §5's ladder (255 steps, a24 = 121665) for the
/// u-coordinate `u` (below 2^256) and the clamped scalar `k`, before the final inversion.
pub fn x25519_ladder(u: [u64; 4], k: [u64; 4]) -> ([u64; 4], [u64; 4]) {
    let x1 = freeze25519(u);
    let (mut x2, mut z2, mut x3, mut z3) = ([1, 0, 0, 0], [0u64; 4], x1, [1, 0, 0, 0]);
    let mut swap = 0;
    for pos in (0..255).rev() {
        let bit = (k[pos / 64] >> (pos % 64)) & 1;
        if swap ^ bit == 1 {
            std::mem::swap(&mut x2, &mut x3);
            std::mem::swap(&mut z2, &mut z3);
        }
        swap = bit;
        let a = add25519(x2, z2);
        let aa = mul25519(a, a);
        let b = sub25519(x2, z2);
        let bb = mul25519(b, b);
        let e = sub25519(aa, bb);
        let c = add25519(x3, z3);
        let d = sub25519(x3, z3);
        let da = mul25519(d, a);
        let cb = mul25519(c, b);
        let t = add25519(da, cb);
        x3 = mul25519(t, t);
        let t = sub25519(da, cb);
        z3 = mul25519(x1, mul25519(t, t));
        x2 = mul25519(aa, bb);
        z2 = mul25519(e, add25519(aa, mul25519(e, [121_665, 0, 0, 0])));
    }
    if swap == 1 {
        std::mem::swap(&mut x2, &mut x3);
        std::mem::swap(&mut z2, &mut z3);
    }
    (freeze25519(x2), freeze25519(z2))
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

    /// RFC 7748 §5.2's first vector: X2 / Z2 is the output u-coordinate.
    #[test]
    fn x25519_ladder_gives_the_rfc_vector() {
        let words = |hex: &str| -> [u64; 4] {
            let b: Vec<u8> = (0..32)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
                .collect();
            std::array::from_fn(|j| u64::from_le_bytes(b[8 * j..8 * j + 8].try_into().unwrap()))
        };
        let mut k = words("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
        k[0] &= !7;
        k[3] &= 0x7fff_ffff_ffff_ffff;
        k[3] |= 0x4000_0000_0000_0000;
        let mut u = words("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
        u[3] &= 0x7fff_ffff_ffff_ffff;
        let (x2, z2) = x25519_ladder(u, k);
        // z2^(p - 2), by square-and-multiply.
        let mut e = P25519;
        e[0] -= 2;
        let (mut acc, mut base) = ([1, 0, 0, 0], z2);
        for w in e {
            for bit in 0..64 {
                if (w >> bit) & 1 == 1 {
                    acc = mul25519(acc, base);
                }
                base = mul25519(base, base);
            }
        }
        let out = mul25519(x2, acc);
        assert_eq!(
            out,
            words("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552")
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
