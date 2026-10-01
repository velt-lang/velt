//! Scalar semantics for the interpreter, on raw bits: a scalar of type `ty` is its bit
//! pattern zero-extended to `u64` (floats: IEEE bits; `F32` in the low 32 bits). Operations
//! are written per concrete Rust type, independently of the constant folder, so comparing
//! interpreted-before vs interpreted-after optimization cross-checks the folder.

use velt_vir::vir::{BinOp, Ty, UnOp};

use super::Trap;

fn mask(ty: Ty) -> u64 {
    match ty.scalar_size().unwrap_or(8) {
        8 => u64::MAX,
        n => (1u64 << (n * 8)) - 1,
    }
}

/// Encode a mathematical integer as raw bits of `ty` (wrapping).
pub(super) fn encode_int(v: i128, ty: Ty) -> u64 {
    (v as u64) & mask(ty)
}

/// Decode raw bits of an integer-like type (`Bool`, `Ptr` included) to its value.
pub(super) fn decode_int(bits: u64, ty: Ty) -> i128 {
    match ty {
        Ty::I8 => i128::from(bits as i8),
        Ty::I16 => i128::from(bits as i16),
        Ty::I32 => i128::from(bits as i32),
        Ty::I64 => i128::from(bits as i64),
        _ => i128::from(bits & mask(ty)),
    }
}

fn decode_float(bits: u64, ty: Ty) -> f64 {
    if ty == Ty::F32 {
        f64::from(f32::from_bits(bits as u32))
    } else {
        f64::from_bits(bits)
    }
}

fn encode_float(x: f64, ty: Ty) -> u64 {
    if ty == Ty::F32 {
        u64::from((x as f32).to_bits())
    } else {
        x.to_bits()
    }
}

/// Integer binary op on concrete Rust types `$s` (signed view) and `$u` (unsigned view).
macro_rules! int_op {
    ($op:expr, $s:ty, $u:ty, $signed:expr, $a:expr, $b:expr) => {{
        let (ua, ub) = ($a as $u, $b as $u);
        let (sa, sb) = (ua as $s, ub as $s);
        let amount = $b as u32;
        let val = |v: $u| Ok(v as u64);
        let flag = |c: bool| Ok(u64::from(c));
        match $op {
            BinOp::Add => val(ua.wrapping_add(ub)),
            BinOp::Sub => val(ua.wrapping_sub(ub)),
            BinOp::Mul => val(ua.wrapping_mul(ub)),
            BinOp::Div | BinOp::Rem if ub == 0 => Err(Trap::DivByZero),
            BinOp::Div if $signed => val(sa.wrapping_div(sb) as $u),
            BinOp::Rem if $signed => val(sa.wrapping_rem(sb) as $u),
            BinOp::Div => val(ua / ub),
            BinOp::Rem => val(ua % ub),
            BinOp::BitAnd => val(ua & ub),
            BinOp::BitOr => val(ua | ub),
            BinOp::BitXor => val(ua ^ ub),
            BinOp::Shl => val(ua.wrapping_shl(amount)),
            BinOp::Shr if $signed => val(sa.wrapping_shr(amount) as $u),
            BinOp::Shr | BinOp::UShr => val(ua.wrapping_shr(amount)),
            BinOp::Eq => flag(ua == ub),
            BinOp::Ne => flag(ua != ub),
            BinOp::Lt if $signed => flag(sa < sb),
            BinOp::Le if $signed => flag(sa <= sb),
            BinOp::Gt if $signed => flag(sa > sb),
            BinOp::Ge if $signed => flag(sa >= sb),
            BinOp::Lt => flag(ua < ub),
            BinOp::Le => flag(ua <= ub),
            BinOp::Gt => flag(ua > ub),
            BinOp::Ge => flag(ua >= ub),
            BinOp::PtrAdd => Err(Trap::Invalid("PtrAdd on a non-pointer".into())),
        }
    }};
}

/// Float binary op at precision `$f`.
macro_rules! float_op {
    ($op:expr, $f:ty, $x:expr, $y:expr, $ty:expr) => {{
        let (x, y) = ($x as $f, $y as $f);
        let val = |v: $f| Ok(encode_float(f64::from(v), $ty));
        let flag = |c: bool| Ok(u64::from(c));
        match $op {
            BinOp::Add => val(x + y),
            BinOp::Sub => val(x - y),
            BinOp::Mul => val(x * y),
            BinOp::Div => val(x / y),
            BinOp::Rem => val(x % y),
            BinOp::Eq => flag(x == y),
            BinOp::Ne => flag(x != y),
            BinOp::Lt => flag(x < y),
            BinOp::Le => flag(x <= y),
            BinOp::Gt => flag(x > y),
            BinOp::Ge => flag(x >= y),
            _ => Err(Trap::Invalid(format!("{:?} on floats", $op))),
        }
    }};
}

/// `a op b` with operand types `ta`, `tb`.
pub(super) fn binary(op: BinOp, ta: Ty, a: u64, tb: Ty, b: u64) -> Result<u64, Trap> {
    if op == BinOp::PtrAdd {
        return Ok(a.wrapping_add(decode_int(b, tb) as u64));
    }
    match ta {
        Ty::I8 => int_op!(op, i8, u8, true, a, b),
        Ty::I16 => int_op!(op, i16, u16, true, a, b),
        Ty::I32 => int_op!(op, i32, u32, true, a, b),
        Ty::I64 => int_op!(op, i64, u64, true, a, b),
        Ty::U8 | Ty::Bool => int_op!(op, i8, u8, false, a, b),
        Ty::U16 => int_op!(op, i16, u16, false, a, b),
        Ty::U32 => int_op!(op, i32, u32, false, a, b),
        Ty::U64 | Ty::Ptr => int_op!(op, i64, u64, false, a, b),
        Ty::F32 => float_op!(
            op,
            f32,
            f32::from_bits(a as u32),
            f32::from_bits(b as u32),
            ta
        ),
        Ty::F64 => float_op!(op, f64, f64::from_bits(a), f64::from_bits(b), ta),
        Ty::Unit | Ty::Agg(_) => Err(Trap::Invalid(format!("{op:?} on {ta:?}"))),
    }
}

/// `op a` with operand type `ty`.
pub(super) fn unary(op: UnOp, ty: Ty, a: u64) -> Result<u64, Trap> {
    match op {
        UnOp::Neg if ty.is_float() => Ok(encode_float(-decode_float(a, ty), ty)),
        UnOp::Neg => Ok(a.wrapping_neg() & mask(ty)),
        UnOp::Not => Ok(a ^ 1),
        UnOp::BitNot => Ok(!a & mask(ty)),
    }
}

/// `a as to` with Rust `as` semantics.
pub(super) fn cast(from: Ty, to: Ty, a: u64) -> Result<u64, Trap> {
    if from.is_float() {
        let x = decode_float(a, from);
        return Ok(match to {
            Ty::F32 | Ty::F64 => encode_float(x, to),
            Ty::I8 => x as i8 as u8 as u64,
            Ty::I16 => x as i16 as u16 as u64,
            Ty::I32 => x as i32 as u32 as u64,
            Ty::I64 => x as i64 as u64,
            Ty::U8 => u64::from(x as u8),
            Ty::U16 => u64::from(x as u16),
            Ty::U32 => u64::from(x as u32),
            Ty::U64 => x as u64,
            _ => return Err(Trap::Invalid(format!("cast {from:?} → {to:?}"))),
        });
    }
    let v = decode_int(a, from);
    Ok(match to {
        Ty::Bool => u64::from(v != 0),
        Ty::F32 if from.is_signed() => u64::from((v as i64 as f32).to_bits()),
        Ty::F32 => u64::from((v as u64 as f32).to_bits()),
        Ty::F64 if from.is_signed() => (v as i64 as f64).to_bits(),
        Ty::F64 => (v as u64 as f64).to_bits(),
        Ty::Unit | Ty::Agg(_) => return Err(Trap::Invalid(format!("cast to {to:?}"))),
        _ => encode_int(v, to),
    })
}

/// Raw bits of a numeric constant of type `ty`.
pub(super) fn int_const(v: i128, ty: Ty) -> u64 {
    match ty {
        Ty::F32 => u64::from((v as f32).to_bits()),
        Ty::F64 => (v as f64).to_bits(),
        _ => encode_int(v, ty),
    }
}

/// Raw bits of a float constant of type `ty`.
pub(super) fn float_const(x: f64, ty: Ty) -> u64 {
    encode_float(x, ty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_semantics() {
        let i8b = |v: i8| v as u8 as u64;
        assert_eq!(
            binary(BinOp::Add, Ty::I8, i8b(127), Ty::I8, 1),
            Ok(i8b(-128))
        );
        assert_eq!(
            binary(BinOp::Div, Ty::I8, i8b(-128), Ty::I8, i8b(-1)),
            Ok(i8b(-128))
        );
        assert_eq!(
            binary(BinOp::Shr, Ty::I8, i8b(-128), Ty::I8, 9),
            Ok(i8b(-64))
        );
        assert_eq!(binary(BinOp::UShr, Ty::I8, i8b(-128), Ty::I8, 7), Ok(1));
        assert_eq!(binary(BinOp::Lt, Ty::U32, 0xFFFF_FFFF, Ty::U32, 0), Ok(0));
        assert_eq!(
            binary(BinOp::Rem, Ty::U32, 1, Ty::U32, 0),
            Err(Trap::DivByZero)
        );
        assert_eq!(
            binary(BinOp::PtrAdd, Ty::Ptr, 100, Ty::I64, (-4i64) as u64),
            Ok(96)
        );
        assert_eq!(unary(UnOp::Neg, Ty::I16, 1), Ok(0xFFFF));
        assert_eq!(cast(Ty::I8, Ty::I64, 0xFF), Ok(u64::MAX));
        assert_eq!(cast(Ty::U8, Ty::I64, 0xFF), Ok(255));
    }

    #[test]
    fn float_semantics() {
        let f = |x: f64| x.to_bits();
        assert_eq!(cast(Ty::F64, Ty::U8, f(300.0)), Ok(255));
        assert_eq!(cast(Ty::F64, Ty::I32, f(f64::NAN)), Ok(0));
        assert_eq!(
            binary(BinOp::Ne, Ty::F64, f(f64::NAN), Ty::F64, f(f64::NAN)),
            Ok(1)
        );
        assert_eq!(float_const(0.1, Ty::F32), u64::from(0.1f32.to_bits()));
    }
}
