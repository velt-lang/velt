//! Compile-time values and evaluation of VIR operators on them, bit-exact with the semantics
//! in `vir.rs` (and the Cranelift backend): wrapping integer arithmetic at the operand width,
//! signedness from the type, shift amounts masked to the width, `MIN / -1 == MIN`, Rust `as`
//! casts (saturating float→int), IEEE floats computed at their own precision.
//! Anything that cannot be folded exactly (division by zero, symbolic addresses in
//! arithmetic, undefined type combinations) yields `None` and is left to run time.

use velt_vir::vir::{BinOp, Const, ExternId, FuncId, StaticId, Ty, UnOp};

/// A known value of a scalar local or operand.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Value {
    /// Integer, `Bool` (0/1) or numeric `Ptr`, normalized to its type's range: sign-extended
    /// for signed types, zero-extended otherwise.
    Int(i128),
    /// Float; `F32` values are exactly representable as `f32`.
    Float(f64),
    /// Symbolic address — can be propagated, not computed on.
    Addr(Symbol),
}

/// The target of an address constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Symbol {
    /// `Const::Static`.
    Static(StaticId),
    /// `Const::Func`.
    Func(FuncId),
    /// `Const::Extern`.
    Extern(ExternId),
}

impl Value {
    /// Identity, used by the dataflow meet (floats compare bitwise so NaN == NaN, 0 != -0).
    pub fn same(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Addr(a), Value::Addr(b)) => a == b,
            _ => false,
        }
    }

    /// Interpret a constant operand of type `ty`.
    pub fn from_const(c: &Const, ty: Ty) -> Option<Value> {
        match (c, ty) {
            (Const::Int(i), Ty::F32) => Some(Value::Float(f64::from(*i as f32))),
            (Const::Int(i), Ty::F64) => Some(Value::Float(*i as f64)),
            (Const::Int(i), t) if int_like(t) => Some(Value::Int(normalize(*i, t))),
            (Const::Float(x), Ty::F32) => Some(Value::Float(f64::from(*x as f32))),
            (Const::Float(x), Ty::F64) => Some(Value::Float(*x)),
            (Const::Bool(b), t) if int_like(t) => Some(Value::Int(normalize(i128::from(*b), t))),
            (Const::Static(id), Ty::Ptr) => Some(Value::Addr(Symbol::Static(*id))),
            (Const::Func(id), Ty::Ptr) => Some(Value::Addr(Symbol::Func(*id))),
            (Const::Extern(id), Ty::Ptr) => Some(Value::Addr(Symbol::Extern(*id))),
            _ => None,
        }
    }

    /// The constant that represents this value at type `ty`.
    pub fn to_const(self, ty: Ty) -> Const {
        match self {
            Value::Int(i) if ty == Ty::Bool => Const::Bool(i != 0),
            Value::Int(i) => Const::Int(i),
            Value::Float(x) => Const::Float(x),
            Value::Addr(Symbol::Static(id)) => Const::Static(id),
            Value::Addr(Symbol::Func(id)) => Const::Func(id),
            Value::Addr(Symbol::Extern(id)) => Const::Extern(id),
        }
    }
}

/// Types with integer semantics (`Bool` behaves as `U8`, `Ptr` as `U64`).
fn int_like(ty: Ty) -> bool {
    ty.is_int() || matches!(ty, Ty::Bool | Ty::Ptr)
}

fn bits(ty: Ty) -> u32 {
    ty.scalar_size().unwrap_or(8) * 8
}

/// Wrap `v` into `ty`'s range.
pub(crate) fn normalize(v: i128, ty: Ty) -> i128 {
    let shift = 128 - bits(ty);
    if ty.is_signed() {
        (v << shift) >> shift
    } else {
        ((v as u128) << shift >> shift) as i128
    }
}

/// Evaluate a unary operator on an operand of type `ty`.
pub(crate) fn unary(op: UnOp, v: Value, ty: Ty) -> Option<Value> {
    match (op, v) {
        (UnOp::Neg, Value::Int(i)) if ty.is_int() => {
            Some(Value::Int(normalize(i.wrapping_neg(), ty)))
        }
        (UnOp::Neg, Value::Float(x)) => Some(Value::Float(-x)),
        (UnOp::Not, Value::Int(i)) if ty == Ty::Bool => Some(Value::Int(i128::from(i == 0))),
        (UnOp::BitNot, Value::Int(i)) if ty.is_int() => Some(Value::Int(normalize(!i, ty))),
        _ => None,
    }
}

/// Evaluate `a op b`, where `ty` is the type of `a`.
pub(crate) fn binary(op: BinOp, a: Value, b: Value, ty: Ty) -> Option<Value> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) if int_like(ty) => int_binary(op, x, y, ty),
        (Value::Float(x), Value::Float(y)) if ty == Ty::F32 => f32_binary(op, x as f32, y as f32),
        (Value::Float(x), Value::Float(y)) if ty == Ty::F64 => f64_binary(op, x, y),
        _ => None,
    }
}

fn int_binary(op: BinOp, x: i128, y: i128, ty: Ty) -> Option<Value> {
    let width = bits(ty);
    let arith = ty.is_int() || ty == Ty::Ptr;
    let bitwise = ty.is_int() || ty == Ty::Bool;
    let int = |v: i128| Some(Value::Int(normalize(v, ty)));
    let flag = |b: bool| Some(Value::Int(i128::from(b)));
    match op {
        BinOp::Add if arith => int(x.wrapping_add(y)),
        BinOp::Sub if arith => int(x.wrapping_sub(y)),
        BinOp::Mul if arith => int(x.wrapping_mul(y)),
        // Operands are normalized, so i128 division is exact; `MIN / -1` wraps via normalize.
        BinOp::Div if arith && y != 0 => int(x / y),
        BinOp::Rem if arith && y != 0 => int(x % y),
        BinOp::BitAnd if bitwise => int(x & y),
        BinOp::BitOr if bitwise => int(x | y),
        BinOp::BitXor if bitwise => int(x ^ y),
        BinOp::Shl | BinOp::Shr | BinOp::UShr if ty.is_int() => {
            let amount = (y as u128 & u128::from(width - 1)) as u32;
            let pattern = (x as u128) & (u128::MAX >> (128 - width));
            int(match op {
                BinOp::Shl => x << amount,
                BinOp::Shr if ty.is_signed() => x >> amount,
                _ => (pattern >> amount) as i128,
            })
        }
        BinOp::Eq => flag(x == y),
        BinOp::Ne => flag(x != y),
        BinOp::Lt => flag(x < y),
        BinOp::Le => flag(x <= y),
        BinOp::Gt => flag(x > y),
        BinOp::Ge => flag(x >= y),
        _ => None,
    }
}

/// Folds a float operation. Written once as a macro so `f32` really computes in `f32`.
macro_rules! float_binary {
    ($name:ident, $t:ty) => {
        fn $name(op: BinOp, x: $t, y: $t) -> Option<Value> {
            let num = |v: $t| Some(Value::Float(f64::from(v)));
            let flag = |b: bool| Some(Value::Int(i128::from(b)));
            match op {
                BinOp::Add => num(x + y),
                BinOp::Sub => num(x - y),
                BinOp::Mul => num(x * y),
                BinOp::Div => num(x / y),
                BinOp::Rem => num(x % y),
                BinOp::Eq => flag(x == y),
                BinOp::Ne => flag(x != y),
                BinOp::Lt => flag(x < y),
                BinOp::Le => flag(x <= y),
                BinOp::Gt => flag(x > y),
                BinOp::Ge => flag(x >= y),
                _ => None,
            }
        }
    };
}
float_binary!(f32_binary, f32);
float_binary!(f64_binary, f64);

/// Evaluate `v as to`, where `from` is the operand type.
pub(crate) fn cast(v: Value, from: Ty, to: Ty) -> Option<Value> {
    if from == to {
        return Some(v);
    }
    match v {
        Value::Int(i) if int_like(from) => int_cast(i, from, to),
        Value::Float(x) if from.is_float() => float_cast(x, to),
        _ => None,
    }
}

fn int_cast(i: i128, from: Ty, to: Ty) -> Option<Value> {
    // Normalized values are exact in i64 (signed) or u64 (unsigned) and Rust's
    // `i64/u64 as f32/f64` rounds exactly like Cranelift's conversions.
    let (s, u) = (i as i64, i as u64);
    Some(match to {
        Ty::Bool => Value::Int(i128::from(i != 0)),
        t if int_like(t) => Value::Int(normalize(i, t)),
        Ty::F32 if from.is_signed() => Value::Float(f64::from(s as f32)),
        Ty::F32 => Value::Float(f64::from(u as f32)),
        Ty::F64 if from.is_signed() => Value::Float(s as f64),
        Ty::F64 => Value::Float(u as f64),
        _ => return None,
    })
}

fn float_cast(x: f64, to: Ty) -> Option<Value> {
    // Rust `as` from float to int saturates and maps NaN to 0, matching `vir.rs`.
    let int = |v: i128| Some(Value::Int(v));
    match to {
        Ty::F32 => Some(Value::Float(f64::from(x as f32))),
        Ty::F64 => Some(Value::Float(x)),
        Ty::I8 => int(i128::from(x as i8)),
        Ty::I16 => int(i128::from(x as i16)),
        Ty::I32 => int(i128::from(x as i32)),
        Ty::I64 => int(i128::from(x as i64)),
        Ty::U8 => int(i128::from(x as u8)),
        Ty::U16 => int(i128::from(x as u16)),
        Ty::U32 => int(i128::from(x as u32)),
        Ty::U64 => int(i128::from(x as u64)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(v: i128) -> Value {
        Value::Int(v)
    }
    fn int_of(v: Option<Value>) -> i128 {
        match v {
            Some(Value::Int(x)) => x,
            other => panic!("expected int, got {other:?}"),
        }
    }

    #[test]
    fn wrapping_and_width() {
        assert_eq!(int_of(binary(BinOp::Add, i(127), i(1), Ty::I8)), -128);
        assert_eq!(int_of(binary(BinOp::Add, i(255), i(1), Ty::U8)), 0);
        assert_eq!(
            int_of(binary(BinOp::Mul, i(u64::MAX as i128), i(2), Ty::U64)),
            (u64::MAX - 1) as i128
        );
        assert_eq!(
            int_of(binary(BinOp::Div, i(i32::MIN as i128), i(-1), Ty::I32)),
            i32::MIN as i128
        );
        assert_eq!(
            int_of(binary(BinOp::Rem, i(i32::MIN as i128), i(-1), Ty::I32)),
            0
        );
        assert_eq!(int_of(binary(BinOp::Div, i(-7), i(2), Ty::I64)), -3);
        assert_eq!(int_of(binary(BinOp::Rem, i(-7), i(2), Ty::I64)), -1);
        assert!(binary(BinOp::Div, i(1), i(0), Ty::I64).is_none());
        assert!(binary(BinOp::Rem, i(1), i(0), Ty::U8).is_none());
        assert_eq!(int_of(unary(UnOp::Neg, i(-128), Ty::I8)), -128);
        assert_eq!(int_of(unary(UnOp::BitNot, i(0), Ty::U16)), 0xFFFF);
    }

    #[test]
    fn shifts_mask_the_amount() {
        assert_eq!(int_of(binary(BinOp::Shl, i(1), i(33), Ty::I32)), 2);
        assert_eq!(int_of(binary(BinOp::Shl, i(1), i(7), Ty::I8)), -128);
        assert_eq!(int_of(binary(BinOp::Shr, i(-16), i(2), Ty::I32)), -4);
        assert_eq!(int_of(binary(BinOp::UShr, i(-16), i(28), Ty::I32)), 15);
        assert_eq!(int_of(binary(BinOp::Shr, i(0x80), i(7), Ty::U8)), 1);
        assert_eq!(
            int_of(binary(BinOp::Shl, i(1), i(-1), Ty::I64)),
            i64::MIN as i128
        );
    }

    #[test]
    fn comparisons_respect_signedness() {
        assert_eq!(int_of(binary(BinOp::Lt, i(-1), i(0), Ty::I32)), 1);
        assert_eq!(int_of(binary(BinOp::Lt, i(0xFFFF_FFFF), i(0), Ty::U32)), 0);
        let nan = Value::Float(f64::NAN);
        assert_eq!(int_of(binary(BinOp::Ne, nan, nan, Ty::F64)), 1);
        assert_eq!(int_of(binary(BinOp::Eq, nan, nan, Ty::F64)), 0);
    }

    #[test]
    fn casts_follow_rust_as() {
        assert_eq!(int_of(cast(i(-1), Ty::I32, Ty::U64)), u64::MAX as i128);
        assert_eq!(int_of(cast(i(0xFFFF_FFFF), Ty::U32, Ty::I64)), 0xFFFF_FFFF);
        assert_eq!(int_of(cast(i(300), Ty::I32, Ty::U8)), 44);
        assert_eq!(int_of(cast(i(2), Ty::I32, Ty::Bool)), 1);
        assert_eq!(
            int_of(cast(Value::Float(1e10), Ty::F64, Ty::I32)),
            i32::MAX as i128
        );
        assert_eq!(int_of(cast(Value::Float(-5.5), Ty::F64, Ty::U8)), 0);
        assert_eq!(int_of(cast(Value::Float(f64::NAN), Ty::F32, Ty::I64)), 0);
        let Some(Value::Float(x)) = cast(i(16_777_217), Ty::I32, Ty::F32) else {
            panic!()
        };
        assert_eq!(x, 16_777_216.0);
        assert!(cast(Value::Float(1.0), Ty::F64, Ty::Bool).is_none());
    }

    #[test]
    fn f32_computes_in_single_precision() {
        let Some(Value::Float(x)) = binary(
            BinOp::Add,
            Value::Float(16_777_216.0),
            Value::Float(1.0),
            Ty::F32,
        ) else {
            panic!()
        };
        assert_eq!(x, 16_777_216.0);
    }

    #[test]
    fn constants_round_trip() {
        let v = Value::from_const(&Const::Int(-1), Ty::U8).unwrap();
        assert_eq!(int_of(Some(v)), 255);
        assert_eq!(
            Value::from_const(&Const::Bool(true), Ty::Bool)
                .unwrap()
                .to_const(Ty::Bool),
            Const::Bool(true)
        );
        assert!(Value::from_const(&Const::Unit, Ty::Unit).is_none());
    }
}
