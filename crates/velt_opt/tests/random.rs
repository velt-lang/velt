//! Property test: random small VIR programs (straight-line arithmetic at every scalar type,
//! reassigned variables, diamonds, bounded loops, helper calls, address-taken locals,
//! runtime-length memset/memmove/memcpy, calls through a vtable static) must
//! stay valid and compute the same result after optimization. Constants are drawn from edge
//! values, so the constant folder is checked against the interpreter's independent
//! implementation of the same semantics.

mod common;

use common::builder::*;
use common::validate::validate;
use velt_opt::interp::{Interp, RecordingHost};
use velt_opt::{optimize, OptLevel};
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

/// Deterministic xorshift generator (no external dependency needed).
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
const EDGE_INTS: [i128; 12] = [
    0,
    1,
    -1,
    2,
    3,
    7,
    127,
    128,
    255,
    65_535,
    i64::MIN as i128,
    i64::MAX as i128,
];
const EDGE_FLOATS: [f64; 9] = [
    0.0,
    -0.0,
    1.5,
    -2.25,
    1e10,
    -1e20,
    3.4e38,
    f64::NAN,
    f64::INFINITY,
];

/// Generates one function body.
struct Gen<'a> {
    rng: &'a mut Rng,
    fb: FuncBuilder,
    cur: BlockId,
    /// Variables assigned on every path to `cur`.
    vars: Vec<(Local, Ty)>,
    helpers: &'a [FuncId],
    /// 16-byte aggregate `{ U64, U64 }` for the memory statements.
    buf: AggId,
    /// Static holding the address of every helper (8 bytes per slot).
    vtable: Option<StaticId>,
}

impl Gen<'_> {
    fn constant(&mut self, ty: Ty) -> Operand {
        match ty {
            F32 | F64 => float(self.rng.pick(&EDGE_FLOATS), ty),
            Bool => boolean(self.rng.below(2) == 1),
            _ => int(
                self.rng.pick(&EDGE_INTS) + self.rng.below(3) as i128 - 1,
                ty,
            ),
        }
    }

    /// An operand of type `ty`: an existing variable, a cast of one, or a constant.
    fn operand(&mut self, ty: Ty) -> Operand {
        let same: Vec<Local> = self
            .vars
            .iter()
            .filter(|v| v.1 == ty)
            .map(|v| v.0)
            .collect();
        match self.rng.below(4) {
            0 | 1 if !same.is_empty() => copy_local(self.rng.pick(&same)),
            2 if !self.vars.is_empty() && ty != Bool => {
                // VIR allows Bool -> int casts only (not Bool -> float).
                let sources: Vec<_> = self
                    .vars
                    .iter()
                    .filter(|v| !(v.1 == Bool && ty.is_float()))
                    .copied()
                    .collect();
                if sources.is_empty() {
                    return self.constant(ty);
                }
                let (src, _) = self.rng.pick(&sources);
                let t = self.fb.local(ty);
                self.fb
                    .assign(self.cur, t, Rvalue::Cast(copy_local(src), ty));
                copy_local(t)
            }
            _ => self.constant(ty),
        }
    }

    /// A fresh local assigned a random computation of a random type.
    fn compute(&mut self) -> (Local, Ty) {
        let ty = self.rng.pick(&TYPES);
        let rv = self.rvalue(ty);
        let l = self.fb.local(ty);
        self.fb.assign(self.cur, l, rv);
        (l, ty)
    }

    fn rvalue(&mut self, ty: Ty) -> Rvalue {
        if ty == Bool {
            let t = self.rng.pick(&TYPES[..10]);
            let op = self.rng.pick(&CMP_OPS);
            let (a, b) = (self.operand(t), self.operand(t));
            return bin(op, a, b);
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
        let mut b = self.operand(ty);
        if matches!(op, BinOp::Div | BinOp::Rem) {
            // Keep divisors nonzero, like lowering's guards would.
            let nz = self.fb.local(ty);
            self.fb
                .assign(self.cur, nz, bin(BinOp::BitOr, b, int(1, ty)));
            b = copy_local(nz);
        }
        bin(op, a, b)
    }

    fn reassign(&mut self) {
        let ints: Vec<(Local, Ty)> = self.vars.iter().copied().filter(|v| v.1 != Bool).collect();
        if ints.is_empty() {
            return;
        }
        let (l, ty) = self.rng.pick(&ints);
        let rv = if ty.is_int() {
            self.int_binary(ty)
        } else {
            self.rvalue(ty)
        };
        self.fb.assign(self.cur, l, rv);
    }

    /// `if (cond) { reassign… } else { reassign… }`
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

    /// `for i in 0..k { reassign… }` with a small constant or variable-derived bound.
    fn counted_loop(&mut self) {
        let (i, c) = (self.fb.local(U8), self.fb.local(Bool));
        let (head, body, exit) = (self.fb.block(), self.fb.block(), self.fb.block());
        let bound = int(self.rng.below(6) as i128, U8);
        self.fb.assign(self.cur, i, Rvalue::Use(int(0, U8)));
        self.fb.goto(self.cur, head);
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

    fn call_helper(&mut self) {
        let Some(&h) = self.helpers.get(self.rng.below(self.helpers.len().max(1))) else {
            return;
        };
        let (a, b) = (self.operand(I64), self.operand(I64));
        let r = self.fb.local(I64);
        self.cur = self.fb.call(self.cur, Callee::Func(h), vec![a, b], Some(r));
        self.vars.push((r, I64));
    }

    /// A U64 operand masked to `0..=mask`, so lengths stay in bounds but vary at run time.
    fn length(&mut self, mask: i128) -> Operand {
        let raw = self.operand(U64);
        let l = self.fb.local(U64);
        self.fb
            .assign(self.cur, l, bin(BinOp::BitAnd, raw, int(mask, U64)));
        copy_local(l)
    }

    /// `memset(&buf, b, 16)`, then a memmove within the buffer (either direction) and a
    /// non-overlapping memcpy of the low half to the high half; both fields become variables.
    fn memory_ops(&mut self) {
        let (buf, p, q, r) = (
            self.fb.local(Ty::Agg(self.buf)),
            self.fb.local(Ptr),
            self.fb.local(Ptr),
            self.fb.local(Ptr),
        );
        self.fb
            .assign(self.cur, p, Rvalue::AddrOf(Place::local(buf)));
        let (byte, len) = (self.operand(U8), int(16, U64));
        self.fb.push(
            self.cur,
            Stmt::MemSet {
                dst: copy_local(p),
                byte,
                len,
            },
        );
        let shift = int(self.rng.below(8) as i128, U64);
        self.fb
            .assign(self.cur, q, bin(BinOp::PtrAdd, copy_local(p), shift));
        let (dst, src) = if self.rng.below(2) == 0 {
            (q, p)
        } else {
            (p, q)
        };
        let len = self.length(7);
        self.fb.push(
            self.cur,
            Stmt::MemCopyDyn {
                dst: copy_local(dst),
                src: copy_local(src),
                len,
                overlapping: true,
            },
        );
        self.fb
            .assign(self.cur, r, bin(BinOp::PtrAdd, copy_local(p), int(8, U64)));
        let len = self.length(7);
        self.fb.push(
            self.cur,
            Stmt::MemCopyDyn {
                dst: copy_local(r),
                src: copy_local(p),
                len,
                overlapping: false,
            },
        );
        for n in 0..2 {
            let x = self.fb.local(U64);
            self.fb
                .assign(self.cur, x, Rvalue::Use(copy_place(field(buf, n))));
            self.vars.push((x, U64));
        }
    }

    /// Load a helper's address from the vtable static and call it indirectly.
    fn call_through_vtable(&mut self) {
        let Some(vtable) = self.vtable else {
            return self.call_helper();
        };
        let (slot, fp, r) = (self.fb.local(Ptr), self.fb.local(Ptr), self.fb.local(I64));
        let offset = int(8 * self.rng.below(self.helpers.len()) as i128, U64);
        let base = Operand::Const(Const::Static(vtable), Ptr);
        self.fb
            .assign(self.cur, slot, bin(BinOp::PtrAdd, base, offset));
        self.fb
            .assign(self.cur, fp, Rvalue::Use(copy_place(deref(slot, Ptr))));
        let callee = Callee::Ptr {
            target: copy_local(fp),
            params: vec![I64, I64],
            ret: I64,
        };
        let (a, b) = (self.operand(I64), self.operand(I64));
        self.cur = self.fb.call(self.cur, callee, vec![a, b], Some(r));
        self.vars.push((r, I64));
    }

    /// `x = v; p = &x; *p = *p + v2;` then x is readable.
    fn through_pointer(&mut self) {
        let (x, p) = (self.fb.local(I64), self.fb.local(Ptr));
        let v = self.operand(I64);
        self.fb.assign(self.cur, x, Rvalue::Use(v));
        self.fb.assign(self.cur, p, Rvalue::AddrOf(Place::local(x)));
        let v2 = self.operand(I64);
        self.fb.push(
            self.cur,
            Stmt::Assign(
                deref(p, I64),
                bin(BinOp::Add, copy_place(deref(p, I64)), v2),
            ),
        );
        self.vars.push((x, I64));
    }

    fn step(&mut self) {
        match self.rng.below(12) {
            0..=4 => {
                let v = self.compute();
                self.vars.push(v);
            }
            5 => self.reassign(),
            6 => self.diamond(),
            7 => self.counted_loop(),
            8 => self.call_helper(),
            9 => self.memory_ops(),
            10 => self.call_through_vtable(),
            _ => self.through_pointer(),
        }
    }

    /// Fold every variable into an I64 checksum and return it.
    fn finish(mut self) -> Function {
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
        }
        self.fb.ret(self.cur, copy_local(acc));
        self.fb.finish()
    }
}

/// What a generated function may use from the rest of the program.
struct Context<'a> {
    helpers: &'a [FuncId],
    buf: AggId,
    vtable: Option<StaticId>,
}

fn function(rng: &mut Rng, name: &str, linkage: Linkage, cx: Context, steps: usize) -> Function {
    let mut fb = FuncBuilder::new(name, &[I64, I64], I64, linkage);
    let cur = fb.block();
    let vars = vec![(Local(0), I64), (Local(1), I64)];
    let mut g = Gen {
        rng,
        fb,
        cur,
        vars,
        helpers: cx.helpers,
        buf: cx.buf,
        vtable: cx.vtable,
    };
    for _ in 0..steps {
        g.step();
    }
    g.finish()
}

fn program(seed: u64) -> Program {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut pb = ProgramBuilder::new();
    let buf = pb.agg("Buf", 16, 8, &[(U64, 0), (U64, 8)]);
    let mut helpers = vec![];
    for k in 0..rng.below(3) {
        let steps = 2 + rng.below(8);
        let known = helpers.clone();
        let cx = Context {
            helpers: &known,
            buf,
            vtable: None,
        };
        let f = function(&mut rng, &format!("h{k}"), Linkage::Internal, cx, steps);
        helpers.push(pb.add(f));
    }
    // Helpers reachable only through the vtable must survive unused-function removal.
    let vtable = (!helpers.is_empty()).then(|| {
        let relocs = (0u32..)
            .zip(&helpers)
            .map(|(i, &h)| (8 * i, Const::Func(h)))
            .collect();
        pb.stat_with(&vec![0; 8 * helpers.len()], 8, relocs)
    });
    let steps = 5 + rng.below(25);
    let cx = Context {
        helpers: &helpers,
        buf,
        vtable,
    };
    let entry = function(&mut rng, "entry", Linkage::Export, cx, steps);
    pb.add(entry);
    pb.finish()
}

fn run(p: &Program, args: &[u64]) -> Result<u64, velt_opt::interp::Trap> {
    Interp::new(p, RecordingHost::default()).call_symbol("entry", args)
}

#[test]
fn random_programs_keep_their_meaning() {
    let inputs: [[u64; 2]; 3] = [[0, 0], [5, u64::MAX], [i64::MIN as u64, 1 << 40]];
    for seed in 0..400 {
        let original = program(seed);
        if let Err(e) = validate(&original) {
            panic!("seed {seed}: generator produced invalid VIR: {e:?}\n{original}");
        }
        for level in [OptLevel::None, OptLevel::Speed] {
            let mut opt = original.clone();
            optimize(&mut opt, level);
            if let Err(e) = validate(&opt) {
                panic!("seed {seed} {level:?}: invalid output: {e:?}\n{original}\n--- optimized ---\n{opt}");
            }
            for args in &inputs {
                let (before, after) = (run(&original, args), run(&opt, args));
                assert!(before.is_ok(), "seed {seed}: original traps: {before:?}");
                assert_eq!(
                    before, after,
                    "seed {seed} {level:?} {args:?}\n{original}\n--- optimized ---\n{opt}"
                );
            }
        }
    }
}

/// Guard against the generator silently losing coverage of the memory statements and vtables.
#[test]
fn generator_covers_memory_statements_and_vtables() {
    let (mut mem, mut vtable_loads) = (0, 0);
    for seed in 0..400 {
        let p = program(seed);
        for s in p
            .funcs
            .iter()
            .flat_map(|f| &f.blocks)
            .flat_map(|b| &b.stmts)
        {
            match s {
                Stmt::MemSet { .. } | Stmt::MemCopyDyn { .. } => mem += 1,
                Stmt::Assign(
                    _,
                    Rvalue::Binary(BinOp::PtrAdd, Operand::Const(Const::Static(_), _), _),
                ) => vtable_loads += 1,
                _ => {}
            }
        }
    }
    assert!(mem > 100 && vtable_loads > 20, "{mem} {vtable_loads}");
}

/// Give every statement and terminator of `p` a distinct source location.
fn with_unique_locs(mut p: Program) -> Program {
    p.files = vec!["gen.vlt".into()];
    let mut line = 0;
    for (fi, f) in p.funcs.iter_mut().enumerate() {
        f.locs = f
            .blocks
            .iter()
            .map(|b| {
                (0..=b.stmts.len())
                    .map(|_| {
                        line += 1;
                        Some(SrcLoc {
                            file: 0,
                            line,
                            col: fi as u32 + 1,
                        })
                    })
                    .collect()
            })
            .collect();
    }
    p
}

/// Source locations (vir.rs invariant 8) survive every pass: still shaped like the blocks,
/// and every location still present comes from the original program (inlined statements keep
/// their callee's locations).
#[test]
fn random_programs_keep_statement_locations() {
    for seed in 0..400 {
        let original = with_unique_locs(program(seed));
        let known: std::collections::HashSet<SrcLoc> = original
            .funcs
            .iter()
            .flat_map(|f| f.locs.iter().flatten().flatten().copied())
            .collect();
        for level in [OptLevel::None, OptLevel::Speed] {
            let mut opt = original.clone();
            optimize(&mut opt, level);
            if let Err(e) = validate(&opt) {
                panic!("seed {seed} {level:?}: invalid output: {e:?}\n{opt}");
            }
            for f in &opt.funcs {
                assert!(
                    !f.locs.is_empty(),
                    "seed {seed}: {} lost its locations",
                    f.symbol
                );
                let stray = f
                    .locs
                    .iter()
                    .flatten()
                    .flatten()
                    .find(|l| !known.contains(l));
                assert!(stray.is_none(), "seed {seed}: invented location {stray:?}");
            }
        }
    }
}
