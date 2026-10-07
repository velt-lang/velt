//! Property test: random `f64` programs (counters, `+ - * / %`, `neg`, diamonds on
//! comparisons, ToInt32, `trunc`, conversions to integers) over edge values (`-0`, NaN, ±∞,
//! 2^53 ± k, fractions, negative dividends) compute bit-identical results before and after
//! `numrep`, alone and in both optimizer pipelines.

use super::*;
use crate::{optimize, OptLevel};

/// Deterministic xorshift generator.
struct Rng(u64);

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

const TWO_53: f64 = 9_007_199_254_740_992.0;
const CONSTS: [f64; 16] = [
    0.0,
    -0.0,
    1.0,
    -1.0,
    2.0,
    3.0,
    -7.0,
    0.5,
    -2.5,
    1000.0,
    2147483647.0,
    -2147483648.0,
    TWO_53,
    TWO_53 - 1.0,
    -TWO_53,
    1e300,
];
const INPUTS: [f64; 14] = [
    0.0,
    -0.0,
    1.0,
    -1.0,
    3.0,
    -5.0,
    2.5,
    -0.5,
    TWO_53,
    TWO_53 + 2.0,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
    4294967296.0,
];
const INT_INPUTS: [i64; 8] = [0, 1, -1, 7, -9, 1 << 40, i64::MAX, i64::MIN];
const OPS: [BinOp; 5] = [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div, BinOp::Rem];
const CMPS: [BinOp; 6] = [
    BinOp::Lt,
    BinOp::Le,
    BinOp::Gt,
    BinOp::Ge,
    BinOp::Eq,
    BinOp::Ne,
];
const VARS: usize = 4;

struct Gen {
    rng: Rng,
    fb: FuncBuilder,
    vars: Vec<Local>,
    /// The loop counter (an `f64`), once inside the loop.
    counter: Option<Local>,
    to_int32: ExternId,
    trunc: ExternId,
}

impl Gen {
    fn operand(&mut self) -> Operand {
        match self.rng.below(5) {
            0 => float(self.rng.pick(&CONSTS), Ty::F64),
            1 if self.counter.is_some() => copy_local(self.counter.unwrap_or(self.vars[0])),
            _ => copy_local(self.vars[self.rng.below(VARS)]),
        }
    }

    /// One random assignment to a variable in block `b`.
    fn assign(&mut self, b: BlockId) {
        let dst = self.vars[self.rng.below(VARS)];
        let rv = match self.rng.below(8) {
            0 => Rvalue::Unary(UnOp::Neg, self.operand()),
            1 => Rvalue::Use(self.operand()),
            _ => {
                let op = self.rng.pick(&OPS);
                bin(op, self.operand(), self.operand())
            }
        };
        self.fb.assign(b, dst, rv);
    }

    /// A few assignments, maybe split by a diamond on a comparison; returns the block to
    /// continue in.
    fn straight(&mut self, mut b: BlockId) -> BlockId {
        for _ in 0..1 + self.rng.below(4) {
            if self.rng.below(4) == 0 {
                let c = self.fb.local(Ty::Bool);
                let op = self.rng.pick(&CMPS);
                let (x, y) = (self.operand(), self.operand());
                self.fb.assign(b, c, bin(op, x, y));
                let (t, e, j) = (self.fb.block(), self.fb.block(), self.fb.block());
                self.fb.branch(b, c, t, e);
                self.assign(t);
                self.fb.goto(t, j);
                self.assign(e);
                self.fb.goto(e, j);
                b = j;
            } else {
                self.assign(b);
            }
        }
        b
    }

    /// `for (k = start; k < bound; k += step) { … }`; returns the exit block.
    fn counted_loop(&mut self, b: BlockId) -> BlockId {
        let k = self.fb.local(Ty::F64);
        let c = self.fb.local(Ty::Bool);
        // (start, bound, step): counters from 0, negative, fractional, and up to 2^53 (where
        // `k + 1` stops growing).
        let (start, bound, step) = self.rng.pick(&[
            (0.0, 4.0, 1.0),
            (1.0, 9.0, 2.0),
            (-3.0, 2.5, 1.0),
            (0.5, 4.0, 0.5),
            (0.0, 0.0, 1.0),
            (TWO_53 - 2.0, TWO_53, 1.0),
        ]);
        self.fb.assign(b, k, Rvalue::Use(float(start, Ty::F64)));
        let (head, body, exit) = (self.fb.block(), self.fb.block(), self.fb.block());
        self.fb.goto(b, head);
        self.fb.assign(
            head,
            c,
            bin(BinOp::Lt, copy_local(k), float(bound, Ty::F64)),
        );
        self.fb.branch(head, c, body, exit);
        self.counter = Some(k);
        let end = self.straight(body);
        self.counter = None;
        let t = self.fb.local(Ty::F64);
        self.fb
            .assign(end, t, bin(BinOp::Add, copy_local(k), float(step, Ty::F64)));
        self.fb.assign(end, k, Rvalue::Use(copy_local(t)));
        self.fb.goto(end, head);
        exit
    }

    /// The result: a variable, its ToInt32, its `trunc`, or its conversion to `i64`, as bits.
    fn finish(mut self, b: BlockId) -> Function {
        let v = copy_local(self.vars[self.rng.below(VARS)]);
        let r = self.fb.local(Ty::F64);
        match self.rng.below(4) {
            0 => {
                let i = self.fb.local(Ty::I32);
                let next = self
                    .fb
                    .call(b, Callee::Extern(self.to_int32), vec![v], Some(i));
                self.fb
                    .assign(next, r, Rvalue::Cast(copy_local(i), Ty::F64));
                self.fb.ret(next, copy_local(r));
            }
            1 => {
                let next = self
                    .fb
                    .call(b, Callee::Extern(self.trunc), vec![v], Some(r));
                self.fb.ret(next, copy_local(r));
            }
            2 => {
                let i = self.fb.local(Ty::I64);
                self.fb.assign(b, i, Rvalue::Cast(v, Ty::I64));
                self.fb.assign(b, r, Rvalue::Cast(copy_local(i), Ty::F64));
                self.fb.ret(b, copy_local(r));
            }
            _ => self.fb.ret(b, v),
        }
        self.fb.finish()
    }
}

fn program(seed: u64) -> Program {
    let mut pb = ProgramBuilder::new();
    let to_int32 = pb.ext(TO_INT32, &[Ty::F64], Ty::I32, false);
    let trunc = pb.ext("velt_rt_math_trunc", &[Ty::F64], Ty::F64, false);
    let mut fb = FuncBuilder::export("f", &[Ty::F64, Ty::F64, Ty::I64], Ty::F64);
    let params = [fb.param(0), fb.param(1), fb.param(2)];
    let vars: Vec<Local> = (0..VARS).map(|_| fb.local(Ty::F64)).collect();
    let mut g = Gen {
        rng: Rng(seed),
        fb,
        vars,
        counter: None,
        to_int32,
        trunc,
    };
    let entry = g.fb.block();
    let small = g.fb.local(Ty::I32);
    g.fb.assign(entry, small, Rvalue::Cast(copy_local(params[2]), Ty::I32));
    for i in 0..VARS {
        let rv = match g.rng.below(4) {
            0 => Rvalue::Use(copy_local(params[i % 2])),
            1 => Rvalue::Cast(copy_local(params[2]), Ty::F64),
            2 => Rvalue::Cast(copy_local(small), Ty::F64),
            _ => Rvalue::Use(float(g.rng.pick(&CONSTS), Ty::F64)),
        };
        g.fb.assign(entry, g.vars[i], rv);
    }
    let mut b = g.straight(entry);
    if g.rng.below(3) != 0 {
        b = g.counted_loop(b);
        b = g.straight(b);
    }
    let f = g.finish(b);
    pb.add(f);
    pb.finish()
}

/// Same bits, or both NaN, or the same trap.
fn same(a: &Result<u64, Trap>, b: &Result<u64, Trap>) -> bool {
    match (a, b) {
        (Ok(x), Ok(y)) => x == y || (f64::from_bits(*x).is_nan() && f64::from_bits(*y).is_nan()),
        (Err(x), Err(y)) => x == y,
        _ => false,
    }
}

fn check(p: &Program, q: &Program, seed: u64, what: &str) {
    for (i, &x) in INPUTS.iter().enumerate() {
        let y = INPUTS[(i * 5 + seed as usize) % INPUTS.len()];
        for n in INT_INPUTS {
            let args = [x.to_bits(), y.to_bits(), n as u64];
            let (want, got) = (call(p, &args), call(q, &args));
            assert!(
                same(&want, &got),
                "seed {seed} ({what}), f({x}, {y}, {n}): {want:?} vs {got:?}\nbefore:\n{}\nafter:\n{}",
                p,
                q
            );
        }
    }
}

#[test]
fn random_f64_programs_keep_their_results() {
    let mut narrowed_any = 0;
    for seed in 1..=4000u64 {
        let p = program(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        crate::testkit::validate::assert_valid(&p);
        let mut q = p.clone();
        if run_program(&mut q) {
            narrowed_any += 1;
        }
        assert_valid(&q);
        check(&p, &q, seed, "numrep");
        if seed % 4 == 0 {
            for level in [OptLevel::None, OptLevel::Speed] {
                let mut o = p.clone();
                optimize(&mut o, level);
                assert_valid(&o);
                check(
                    &p,
                    &o,
                    seed,
                    if level == OptLevel::None {
                        "debug"
                    } else {
                        "release"
                    },
                );
            }
        }
    }
    assert!(narrowed_any > 1000, "only {narrowed_any} programs changed");
}
