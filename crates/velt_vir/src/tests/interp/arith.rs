//! Scalar semantics for the interpreter: raw-bit integer/float helpers, binary operators and
//! Rust-`as` casts, mirroring what backends must implement for VIR.

use crate::vir::{BinOp, Ty};

pub(super) fn mask(bits: u32) -> u64 {
    if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

pub(super) fn bits_of(t: Ty) -> u32 {
    t.scalar_size().expect("scalar") * 8
}

/// Interpret raw bits as an integer of type `t` (sign-extending signed types).
pub(super) fn to_i128(v: u64, t: Ty) -> i128 {
    let b = bits_of(t);
    let v = v & mask(b);
    if t.is_signed() && (v >> (b - 1)) & 1 == 1 {
        v as i128 - (1i128 << b)
    } else {
        v as i128
    }
}

/// Truncate an integer to the raw bits of type `t`.
pub(super) fn from_i128(v: i128, t: Ty) -> u64 {
    (v as u64) & mask(bits_of(t))
}

pub(super) fn to_f(v: u64, t: Ty) -> f64 {
    if t == Ty::F32 {
        f32::from_bits(v as u32) as f64
    } else {
        f64::from_bits(v)
    }
}

pub(super) fn from_f(f: f64, t: Ty) -> u64 {
    if t == Ty::F32 {
        (f as f32).to_bits() as u64
    } else {
        f.to_bits()
    }
}

/// Evaluate `a op b` on raw bits; `t` is the lhs type, `tb` the rhs type (differs for shifts).
pub(super) fn binop(op: BinOp, a: u64, b: u64, t: Ty, tb: Ty) -> u64 {
    if t.is_float() {
        float_binop(op, to_f(a, t), to_f(b, t), t)
    } else if t == Ty::Bool || t == Ty::Ptr {
        match op {
            BinOp::Eq => (a == b) as u64,
            BinOp::Ne => (a != b) as u64,
            BinOp::BitAnd => a & b,
            BinOp::BitOr => a | b,
            BinOp::BitXor => a ^ b,
            BinOp::PtrAdd => a.wrapping_add(b),
            _ => panic!("interp: {op:?} on {t:?}"),
        }
    } else {
        int_binop(op, a, b, t, tb)
    }
}

fn float_binop(op: BinOp, x: f64, y: f64, t: Ty) -> u64 {
    match op {
        BinOp::Add => from_f(x + y, t),
        BinOp::Sub => from_f(x - y, t),
        BinOp::Mul => from_f(x * y, t),
        BinOp::Div => from_f(x / y, t),
        BinOp::Rem => from_f(x % y, t),
        BinOp::Eq => (x == y) as u64,
        BinOp::Ne => (x != y) as u64,
        BinOp::Lt => (x < y) as u64,
        BinOp::Le => (x <= y) as u64,
        BinOp::Gt => (x > y) as u64,
        BinOp::Ge => (x >= y) as u64,
        _ => panic!("interp: {op:?} on float"),
    }
}

fn int_binop(op: BinOp, a: u64, b: u64, t: Ty, tb: Ty) -> u64 {
    use BinOp::*;
    let bits = bits_of(t);
    let (x, y) = (to_i128(a, t), to_i128(b, t));
    match op {
        Add => from_i128(x.wrapping_add(y), t),
        Sub => from_i128(x.wrapping_sub(y), t),
        Mul => from_i128(x.wrapping_mul(y), t),
        Div | Rem => {
            // Lowering must guard both cases; real backends would trap here.
            assert!(y != 0, "interp: division by zero reached the backend");
            let min = -(1i128 << (bits - 1));
            assert!(
                !(t.is_signed() && x == min && y == -1),
                "interp: MIN / -1 reached the backend"
            );
            from_i128(if op == Div { x / y } else { x % y }, t)
        }
        BitAnd => a & b & mask(bits),
        BitOr => (a | b) & mask(bits),
        BitXor => (a ^ b) & mask(bits),
        Shl => from_i128(x << shift(b, tb, bits), t),
        Shr => from_i128(x >> shift(b, tb, bits), t),
        UShr => (a & mask(bits)) >> shift(b, tb, bits),
        Eq => (x == y) as u64,
        Ne => (x != y) as u64,
        Lt => (x < y) as u64,
        Le => (x <= y) as u64,
        Gt => (x > y) as u64,
        Ge => (x >= y) as u64,
        PtrAdd => panic!("interp: PtrAdd on int"),
    }
}

/// Shift amounts are masked to the operand width (like Rust's wrapping shifts and Cranelift).
fn shift(b: u64, tb: Ty, bits: u32) -> u32 {
    (to_i128(b, tb) as u32) & (bits - 1)
}

/// Rust `as` semantics: int↔int truncate/extend, float→int saturating (NaN → 0), Bool/Ptr → int.
pub(super) fn cast(v: u64, from: Ty, to: Ty) -> u64 {
    let from_is_int = from.is_int() || from == Ty::Bool || from == Ty::Ptr;
    let to_is_int = to.is_int() || to == Ty::Ptr;
    let int_value = || {
        if from.is_int() {
            to_i128(v, from)
        } else {
            v as i128
        }
    };
    match (from_is_int, to_is_int) {
        (true, true) if to == Ty::Ptr => int_value() as u64,
        (true, true) => from_i128(int_value(), to),
        (true, false) => from_f(int_value() as f64, to),
        (false, false) => from_f(to_f(v, from), to),
        (false, true) => {
            let bits = bits_of(to);
            let (lo, hi) = if to.is_signed() {
                (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
            } else {
                (0, (1i128 << bits) - 1)
            };
            from_i128((to_f(v, from) as i128).clamp(lo, hi), to)
        }
    }
}
