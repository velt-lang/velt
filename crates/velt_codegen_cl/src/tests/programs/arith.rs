//! Arithmetic programs: integer/float operators and casts at every width.

use super::*;
use crate::tests::*;
use velt_vir::vir::Ty::*;

pub(crate) fn int_ops() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    // (op, type a, a, type b, b, expected)
    let cases: Vec<(BinOp, Ty, i128, Ty, i128, &str)> = vec![
        (BinOp::Div, I32, -7, I32, 3, "-2"),
        (BinOp::Rem, I32, -7, I32, 3, "-1"),
        (BinOp::Div, U32, 4_000_000_000, U32, 3, "1333333333"),
        (BinOp::Rem, U32, 4_000_000_000, U32, 7, "3"),
        (BinOp::Add, U8, 250, U8, 10, "4"),
        (BinOp::Sub, U8, 3, U8, 5, "254"),
        (BinOp::Mul, I8, 100, I8, 3, "44"),
        (BinOp::Add, I16, 32767, I16, 1, "-32768"),
        (BinOp::Shr, I32, -16, I32, 2, "-4"),
        (BinOp::UShr, I32, -16, I32, 2, "1073741820"),
        (BinOp::Shr, U32, 0xFFFF_FFF0, U32, 2, "1073741820"),
        (BinOp::Shl, I32, 1, I32, 33, "2"),
        (BinOp::Shl, I8, 1, I8, 7, "-128"),
        (BinOp::Shr, I64, -1, I64, 65, "-1"),
        (BinOp::Shl, I64, 1, U8, 40, "1099511627776"),
        (BinOp::Shr, I8, -128, I64, 3, "-16"),
        (BinOp::UShr, I8, -128, I64, 3, "16"),
        (
            BinOp::Div,
            I64,
            i64::MIN as i128,
            I64,
            -1,
            "-9223372036854775808",
        ),
        (BinOp::Rem, I64, i64::MIN as i128, I64, -1, "0"),
        (BinOp::Div, I8, -128, I8, -1, "-128"),
        (BinOp::Div, I64, 7, I64, -1, "-7"),
        (BinOp::Rem, I32, 7, I32, -1, "0"),
        (BinOp::Rem, I16, -7, I16, 2, "-1"),
        (BinOp::Div, U8, 200, U8, 7, "28"),
        (
            BinOp::Mul,
            U64,
            u64::MAX as i128,
            U64,
            2,
            "18446744073709551614",
        ),
        (BinOp::BitXor, U16, 0xF0F0, U16, 0xFFFF, "3855"),
        (BinOp::BitAnd, I32, 0xFF00, I32, 0x0FF0, "3840"),
        (BinOp::BitOr, U8, 0x81, U8, 0x18, "153"),
        (BinOp::Lt, I32, -1, I32, 1, "true"),
        (BinOp::Lt, U32, 0xFFFF_FFFF, U32, 1, "false"),
        (BinOp::Ge, I8, -5, I8, -5, "true"),
        (BinOp::Gt, U8, 200, U8, 100, "true"),
        (BinOp::Gt, I8, -56, I8, 100, "false"),
        (
            BinOp::Eq,
            U64,
            u64::MAX as i128,
            U64,
            u64::MAX as i128,
            "true",
        ),
        (BinOp::Ne, I16, 1, I16, 1, "false"),
        (BinOp::Le, U16, 65535, U16, 0, "false"),
        (BinOp::Eq, Bool, 1, Bool, 1, "true"),
        (BinOp::BitXor, Bool, 1, Bool, 1, "false"),
        (BinOp::Eq, Ptr, 0, Ptr, 0, "true"),
        (BinOp::Ne, Ptr, 8, Ptr, 0, "true"),
        (BinOp::PtrAdd, Ptr, 100, I64, -4, "96"),
    ];
    let unary: Vec<(UnOp, Ty, i128, &str)> = vec![
        (UnOp::Neg, I32, i32::MIN as i128, "-2147483648"),
        (UnOp::Neg, I64, 5, "-5"),
        (UnOp::BitNot, U8, 0, "255"),
        (UnOp::BitNot, I16, 0, "-1"),
        (UnOp::Not, Bool, 1, "false"),
        (UnOp::Not, Bool, 0, "true"),
    ];
    let mut fns = vec![];
    for &(op, ta, a, tb, b, exp) in &cases {
        let (f, rty) = binary_fn(&mut pb, op, ta, tb);
        fns.push((f, vec![int(a, ta), int(b, tb)], rty, exp));
    }
    for &(op, ty, a, exp) in &unary {
        let f = unary_fn(&mut pb, op, ty);
        fns.push((f, vec![int(a, ty)], ty, exp));
    }
    let (mut fb, b0) = main_fb();
    let mut cs = Cases {
        o: Out {
            fb: &mut fb,
            rt: &rt,
            cur: b0,
        },
        expected: String::new(),
    };
    for (f, args, rty, exp) in fns {
        cs.call(f, args, rty, exp);
    }
    let (cur, expected) = (cs.o.cur, cs.expected);
    pb.add(finish_main(fb, cur, 3));
    TestProgram {
        name: "int_ops",
        program: pb.p,
        stdout: expected,
        exit: 3,
    }
}

pub(crate) fn float_ops() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let nan = f64::NAN;
    let cases: Vec<(BinOp, Ty, f64, f64, &str)> = vec![
        (BinOp::Add, F64, 0.1, 0.2, "0.30000000000000004"),
        (BinOp::Sub, F64, 1.0, 3.0, "-2"),
        (BinOp::Mul, F32, 1.5, 4.0, "6"),
        (BinOp::Div, F64, 1.0, 0.0, "inf"),
        (BinOp::Div, F64, -1.0, 0.0, "-inf"),
        (BinOp::Rem, F64, 7.5, 2.0, "1.5"),
        (BinOp::Rem, F64, -7.5, 2.0, "-1.5"),
        (BinOp::Rem, F32, 7.5, 2.0, "1.5"),
        (BinOp::Eq, F64, nan, nan, "false"),
        (BinOp::Ne, F64, nan, nan, "true"),
        (BinOp::Ne, F64, 1.0, 1.0, "false"),
        (BinOp::Lt, F64, nan, 1.0, "false"),
        (BinOp::Le, F64, nan, 1.0, "false"),
        (BinOp::Gt, F64, nan, 1.0, "false"),
        (BinOp::Ge, F64, 1.0, nan, "false"),
        (BinOp::Lt, F64, 1.0, 2.0, "true"),
        (BinOp::Ge, F32, 2.0, 2.0, "true"),
        (BinOp::Eq, F32, nan, nan, "false"),
        (BinOp::Ne, F32, nan, 0.0, "true"),
    ];
    let mut fns = vec![];
    for &(op, ty, a, b, exp) in &cases {
        let (f, rty) = binary_fn(&mut pb, op, ty, ty);
        fns.push((f, vec![operand(a, ty), operand(b, ty)], rty, exp));
    }
    let neg = unary_fn(&mut pb, UnOp::Neg, F64);
    fns.push((neg, vec![float(0.0, F64)], F64, "-0"));
    let (mut fb, b0) = main_fb();
    let mut cs = Cases {
        o: Out {
            fb: &mut fb,
            rt: &rt,
            cur: b0,
        },
        expected: String::new(),
    };
    for (f, args, rty, exp) in fns {
        cs.call(f, args, rty, exp);
    }
    let (cur, expected) = (cs.o.cur, cs.expected);
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "float_ops",
        program: pb.p,
        stdout: expected,
        exit: 0,
    }
}

pub(crate) fn casts() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let cases: Vec<(Ty, Operand, Ty, &str)> = vec![
        (I32, int(300, I32), U8, "44"),
        (I32, int(-1, I32), U64, "18446744073709551615"),
        (I8, int(-1, I8), U32, "4294967295"),
        (U8, int(200, U8), I8, "-56"),
        (U8, int(200, U8), I64, "200"),
        (I16, int(-300, I16), I64, "-300"),
        (U64, int(u64::MAX as i128, U64), I32, "-1"),
        (F64, float(1e10, F64), I32, "2147483647"),
        (F64, float(-1e10, F64), I32, "-2147483648"),
        (F64, float(-1.5, F64), U8, "0"),
        (F64, float(f64::NAN, F64), I32, "0"),
        (F64, float(f64::NAN, F64), U8, "0"),
        (F64, float(300.7, F64), U8, "255"),
        (F64, float(-200.0, F64), I8, "-128"),
        (F64, float(200.0, F64), I8, "127"),
        (F64, float(-3.9, F64), I16, "-3"),
        (F64, float(3.9, F64), I64, "3"),
        (F64, float(1e30, F64), U64, "18446744073709551615"),
        (F64, float(f64::INFINITY, F64), I64, "9223372036854775807"),
        (F32, float(-1e20, F32), I64, "-9223372036854775808"),
        (F32, float(70000.0, F32), U16, "65535"),
        (U64, int(u64::MAX as i128, U64), F64, "18446744073709552000"),
        (I32, int(-5, I32), F64, "-5"),
        (I8, int(-5, I8), F32, "-5"),
        (U8, int(255, U8), F64, "255"),
        (U32, int(4_000_000_000, U32), F32, "4000000000"),
        (Bool, Operand::Const(Const::Bool(true), Bool), I32, "1"),
        (Bool, Operand::Const(Const::Bool(true), Bool), U64, "1"),
        (F64, float(0.1, F64), F32, "0.10000000149011612"),
        (F32, float(0.5, F32), F64, "0.5"),
        (I64, int(4096, I64), Ptr, "4096"),
        (Ptr, int(-1, Ptr), I32, "-1"),
        (Ptr, int(77, Ptr), U8, "77"),
        (I32, int(0, I32), Bool, "false"),
        (I32, int(7, I32), Bool, "true"),
    ];
    let mut fns = vec![];
    for (from, arg, to, exp) in cases {
        let f = cast_fn(&mut pb, from, to);
        fns.push((f, vec![arg], to, exp));
    }
    let (mut fb, b0) = main_fb();
    let mut cs = Cases {
        o: Out {
            fb: &mut fb,
            rt: &rt,
            cur: b0,
        },
        expected: String::new(),
    };
    for (f, args, rty, exp) in fns {
        cs.call(f, args, rty, exp);
    }
    let (cur, expected) = (cs.o.cur, cs.expected);
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "casts",
        program: pb.p,
        stdout: expected,
        exit: 0,
    }
}

/// Runtime math functions that the backend emits as instructions (the link harness defines
/// the symbols to return NaN, so a call instead of the instruction shows in the output).
/// The math operations (`math_intrinsics`), each in a function of its own.
pub(crate) fn math() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let intrinsics = math_intrinsics(&mut pb);
    let (mut fb, b0) = main_fb();
    let mut cs = Cases {
        o: Out {
            fb: &mut fb,
            rt: &rt,
            cur: b0,
        },
        expected: String::new(),
    };
    for (f, args, rty, exp) in intrinsics {
        cs.call(f, args, rty, exp);
    }
    let (cur, expected) = (cs.o.cur, cs.expected);
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "math",
        program: pb.p,
        stdout: expected,
        exit: 0,
    }
}

/// Functions computing one math operation each as an `Rvalue::Intrinsic` (#1981), with their
/// arguments and the line they print: inline where Cranelift has the instruction, the runtime
/// function for `pow` and ToInt32's slow path.
fn math_intrinsics(pb: &mut ProgramBuilder) -> Vec<(FuncId, Vec<Operand>, Ty, &'static str)> {
    use velt_vir::math::MathOp as M;
    let f = |x: f64| float(x, F64);
    let cases = [
        (M::Sqrt, vec![f(2.0)], "1.4142135623730951"),
        (M::Floor, vec![f(-2.5)], "-3"),
        (M::Ceil, vec![f(-2.5)], "-2"),
        (M::Trunc, vec![f(-2.7)], "-2"),
        (M::Round, vec![f(-2.5)], "-2"),
        (M::Round, vec![f(2.5)], "3"),
        (M::Abs, vec![f(-0.5)], "0.5"),
        (M::Clz32, vec![int(1, I32)], "31"),
        (M::Clz64, vec![int(1, U64)], "63"),
        (M::Ctz64, vec![int(8, U64)], "3"),
        (M::Popcount64, vec![int(255, U64)], "8"),
        (
            M::Umulh,
            vec![int(i128::from(u64::MAX), U64), int(2, U64)],
            "1",
        ),
        (M::ToInt32, vec![f(4_294_967_297.0)], "1"),
        // Beyond 2^63: the runtime's ToInt32 on the cold path.
        (M::ToInt32, vec![f(1e20)], "1661992960"),
        (M::MulInt32, vec![int(-3, I32), int(7, I32)], "-21"),
        (M::AddInt32, vec![int(5, I32), f(2.5)], "7"),
        (M::PowI64, vec![int(3, I64), int(4, I64)], "81"),
        (M::PowF64, vec![f(2.0), f(10.0)], "1024"),
    ];
    let mut out = vec![];
    for (op, args, exp) in cases {
        let (params, ret) = op.signature();
        if !pb.p.externs.iter().any(|e| e.symbol == op.symbol()) {
            pb.ext(op.symbol(), params, ret, false);
        }
        let n = pb.p.funcs.len();
        let mut fb = FuncBuilder::internal(&format!("rv{n}_{}", op.name()), params, ret);
        let r = fb.local(ret);
        let b = fb.block();
        let operands = (0..params.len() as u32)
            .map(|i| copy_local(Local(i)))
            .collect();
        fb.assign(b, r, Rvalue::Intrinsic(Intrinsic::Math(op), operands));
        fb.term(b, Terminator::Return(copy_local(r)));
        out.push((pb.add(fb.finish()), args, ret, exp));
    }
    out
}
