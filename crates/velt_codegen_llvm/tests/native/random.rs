//! Random VIR programs stressing the operator/cast semantics a backend must get exactly right:
//! arithmetic at every scalar type with edge constants, shifts by out-of-range amounts,
//! signed division by -1, float→int saturation (NaN, ±inf, huge values), int↔float, comparisons
//! (NaN), diamonds, bounded loops, switches, address-taken locals and aggregate fields. Every
//! value is folded into an `I64` checksum and printed through `write_i64`.

use crate::common::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

/// Deterministic xorshift generator.
pub struct Rng(pub u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }
}

const TYPES: [Ty; 11] = [I8, I16, I32, I64, U8, U16, U32, U64, F32, F64, Bool];
const INT_OPS: [BinOp; 11] = [
    BinOp::Add,
    BinOp::Sub,
    BinOp::Mul,
    BinOp::Div,
    BinOp::Rem,
    BinOp::BitAnd,
    BinOp::BitOr,
    BinOp::BitXor,
    BinOp::Shl,
    BinOp::Shr,
    BinOp::UShr,
];
const FLOAT_OPS: [BinOp; 5] = [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div, BinOp::Rem];
const CMP_OPS: [BinOp; 6] = [
    BinOp::Eq,
    BinOp::Ne,
    BinOp::Lt,
    BinOp::Le,
    BinOp::Gt,
    BinOp::Ge,
];
const EDGE_INTS: [i128; 13] = [
    0,
    1,
    -1,
    2,
    7,
    8,
    63,
    64,
    127,
    255,
    65_535,
    i64::MIN as i128,
    i64::MAX as i128,
];
const EDGE_FLOATS: [f64; 11] = [
    0.0,
    -0.0,
    0.5,
    -1.5,
    2.75,
    1e10,
    -1e20,
    3.4e38,
    1e300,
    f64::NAN,
    f64::INFINITY,
];

struct Gen<'a> {
    rng: &'a mut Rng,
    fb: FuncBuilder,
    cur: BlockId,
    vars: Vec<(Local, Ty)>,
    pair: AggId,
}

impl Gen<'_> {
    fn constant(&mut self, ty: Ty) -> Operand {
        match ty {
            F32 | F64 => {
                let sign = if self.rng.below(2) == 0 { 1.0 } else { -1.0 };
                float(sign * self.rng.pick(&EDGE_FLOATS), ty)
            }
            Bool => boolean(self.rng.below(2) == 1),
            _ => int(
                self.rng.pick(&EDGE_INTS) + self.rng.below(3) as i128 - 1,
                ty,
            ),
        }
    }

    /// An existing variable of `ty`, a cast of another variable, or a constant.
    fn operand(&mut self, ty: Ty) -> Operand {
        let same: Vec<Local> = self
            .vars
            .iter()
            .filter(|v| v.1 == ty)
            .map(|v| v.0)
            .collect();
        match self.rng.below(4) {
            0 | 1 if !same.is_empty() => copy_local(self.rng.pick(&same)),
            2 if ty != Bool => {
                let sources: Vec<(Local, Ty)> = self
                    .vars
                    .iter()
                    .filter(|v| !(v.1 == Bool && ty.is_float()))
                    .copied()
                    .collect();
                let (src, _) = self.rng.pick(&sources);
                let t = self.fb.local(ty);
                self.fb
                    .assign(self.cur, t, Rvalue::Cast(copy_local(src), ty));
                copy_local(t)
            }
            _ => self.constant(ty),
        }
    }

    fn rvalue(&mut self, ty: Ty) -> Rvalue {
        if ty == Bool {
            if self.rng.below(4) == 0 {
                let src = self.rng.pick(&TYPES[..8]);
                return Rvalue::Cast(self.operand(src), Bool);
            }
            let t = self.rng.pick(&TYPES[..10]);
            let (a, b) = (self.operand(t), self.operand(t));
            return bin(self.rng.pick(&CMP_OPS), a, b);
        }
        match self.rng.below(5) {
            0 => Rvalue::Use(self.operand(ty)),
            1 if ty.is_int() => {
                Rvalue::Unary(self.rng.pick(&[UnOp::Neg, UnOp::BitNot]), self.operand(ty))
            }
            1 => Rvalue::Unary(UnOp::Neg, self.operand(ty)),
            _ if ty.is_float() => {
                let op = self.rng.pick(&FLOAT_OPS);
                bin(op, self.operand(ty), self.operand(ty))
            }
            _ => self.int_binary(ty),
        }
    }

    fn int_binary(&mut self, ty: Ty) -> Rvalue {
        let op = self.rng.pick(&INT_OPS);
        let a = self.operand(ty);
        let mut b = if matches!(op, BinOp::Shl | BinOp::Shr | BinOp::UShr) {
            // Shift amounts of any int type, often out of range.
            let amount_ty = self.rng.pick(&TYPES[..8]);
            self.operand(amount_ty)
        } else {
            self.operand(ty)
        };
        if matches!(op, BinOp::Div | BinOp::Rem) {
            // Nonzero divisors, as lowering guarantees; `x | 1` keeps -1 (MIN / -1) possible.
            let nz = self.fb.local(ty);
            self.fb
                .assign(self.cur, nz, bin(BinOp::BitOr, b, int(1, ty)));
            b = copy_local(nz);
        }
        bin(op, a, b)
    }

    fn compute(&mut self) {
        let ty = self.rng.pick(&TYPES);
        let rv = self.rvalue(ty);
        let l = self.fb.local(ty);
        self.fb.assign(self.cur, l, rv);
        self.vars.push((l, ty));
    }

    fn reassign(&mut self) {
        let (l, ty) = self.rng.pick(&self.vars.clone());
        let rv = self.rvalue(ty);
        self.fb.assign(self.cur, l, rv);
    }

    fn diamond(&mut self) {
        let c = self.fb.local(Bool);
        let rv = self.rvalue(Bool);
        self.fb.assign(self.cur, c, rv);
        let (then, els, join) = (self.fb.block(), self.fb.block(), self.fb.block());
        self.fb.branch(self.cur, c, then, els);
        for arm in [then, els] {
            self.cur = arm;
            for _ in 0..self.rng.below(3) {
                self.reassign();
            }
            self.fb.goto(self.cur, join);
        }
        self.cur = join;
    }

    fn counted_loop(&mut self) {
        let (i, c) = (self.fb.local(U8), self.fb.local(Bool));
        let (head, body, exit) = (self.fb.block(), self.fb.block(), self.fb.block());
        self.fb.assign(self.cur, i, Rvalue::Use(int(0, U8)));
        self.fb.goto(self.cur, head);
        let bound = int(self.rng.below(6) as i128, U8);
        self.fb
            .assign(head, c, bin(BinOp::Lt, copy_local(i), bound));
        self.fb.branch(head, c, body, exit);
        self.cur = body;
        for _ in 0..1 + self.rng.below(3) {
            self.reassign();
        }
        self.fb
            .assign(self.cur, i, bin(BinOp::Add, copy_local(i), int(1, U8)));
        self.fb.goto(self.cur, head);
        self.cur = exit;
    }

    /// `switch (v & 3) { 0 => reassign, 1|-1… => reassign, _ => }` on a random int type.
    fn switch(&mut self) {
        let ty = self.rng.pick(&TYPES[..8]);
        let v = self.fb.local(ty);
        let a = self.operand(ty);
        self.fb
            .assign(self.cur, v, bin(BinOp::BitAnd, a, int(3, ty)));
        let (b0, b1, join) = (self.fb.block(), self.fb.block(), self.fb.block());
        let cases = vec![(0, b0), (if ty.is_signed() { -1 } else { 1 }, b1), (2, b1)];
        self.fb.term(
            self.cur,
            Terminator::Switch {
                value: copy_local(v),
                cases,
                default: join,
            },
        );
        for arm in [b0, b1] {
            self.cur = arm;
            self.reassign();
            self.fb.goto(self.cur, join);
        }
        self.cur = join;
    }

    /// `x = v; p = &x; *p = *p + v2` and a `{ I64, F64 }` aggregate written and read by field.
    fn memory(&mut self) {
        let (x, p) = (self.fb.local(I64), self.fb.local(Ptr));
        let v = self.operand(I64);
        self.fb.assign(self.cur, x, Rvalue::Use(v));
        self.fb.assign(self.cur, p, Rvalue::AddrOf(Place::local(x)));
        let v2 = self.operand(I64);
        let add = bin(BinOp::Add, copy_place(deref(p, I64)), v2);
        self.fb.push(self.cur, Stmt::Assign(deref(p, I64), add));
        self.vars.push((x, I64));

        let (agg, copy, f) = (
            self.fb.local(Ty::Agg(self.pair)),
            self.fb.local(Ty::Agg(self.pair)),
            self.fb.local(F64),
        );
        let fields = vec![self.operand(I64), self.operand(F64)];
        self.fb
            .assign(self.cur, agg, Rvalue::Aggregate(self.pair, fields));
        self.fb.assign(self.cur, copy, Rvalue::Use(copy_local(agg)));
        self.fb
            .assign(self.cur, f, Rvalue::Use(copy_place(field(copy, 1))));
        self.vars.push((f, F64));
    }

    fn step(&mut self) {
        match self.rng.below(12) {
            0..=4 => self.compute(),
            5 => self.reassign(),
            6 => self.diamond(),
            7 => self.counted_loop(),
            8 => self.switch(),
            _ => self.memory(),
        }
    }

    /// Print every variable's `I64` conversion, fold them into a checksum and return it.
    fn finish(mut self, write_i64: ExternId) -> Function {
        let (acc, w) = (self.fb.local(I64), self.fb.local(I64));
        self.fb.assign(self.cur, acc, Rvalue::Use(int(0, I64)));
        for (l, _) in self.vars.clone() {
            self.fb
                .assign(self.cur, w, Rvalue::Cast(copy_local(l), I64));
            self.fb.assign(
                self.cur,
                acc,
                bin(BinOp::Mul, copy_local(acc), int(31, I64)),
            );
            self.fb.assign(
                self.cur,
                acc,
                bin(BinOp::BitXor, copy_local(acc), copy_local(w)),
            );
            self.cur = self.fb.call(
                self.cur,
                Callee::Extern(write_i64),
                vec![copy_local(w)],
                None,
            );
        }
        self.fb.ret(self.cur, copy_local(acc));
        self.fb.finish()
    }
}

/// A random program with an exported `entry(I64, I64) -> I64`.
pub fn program(seed: u64) -> Program {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut pb = ProgramBuilder::new();
    let write_i64 = pb.ext("write_i64", &[I64], Unit, false);
    let pair = pb.agg("Pair", 16, 8, &[(I64, 0), (F64, 8)]);
    let mut fb = FuncBuilder::export("entry", &[I64, I64], I64);
    let cur = fb.block();
    let steps = 5 + rng.below(30);
    let mut g = Gen {
        rng: &mut rng,
        fb,
        cur,
        vars: vec![(Local(0), I64), (Local(1), I64)],
        pair,
    };
    for _ in 0..steps {
        g.step();
    }
    pb.add(g.finish(write_i64));
    pb.finish()
}
