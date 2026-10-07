//! Test-only VIR interpreter with an emulated `velt_rt` (rt_abi.md). Lets us check lowered programs
//! against the golden `.out` files without a backend, and detect leaks / double frees / bad frees.
//!
//! Memory model: one byte-addressed heap (statics + runtime allocations, never reused, freed bytes
//! poisoned) and a stack region at `STACK_BASE` holding one slot per local; scalars are raw `u64`
//! bits interpreted by their VIR type (arith.rs).

mod arith;
mod async_rt;
mod cancel;
mod drops;
mod http;
mod json;
mod local;
mod net;
mod rt;
mod strbuf;

use std::collections::HashMap;

use crate::vir::*;
use arith::{binop, cast, from_f, from_i128, mask, to_f, to_i128};
pub(super) use cancel::{poll_then_drop, Arg};

/// Observable result of running a program.
pub(super) struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    /// Heap allocations still live at a normal exit (leaks).
    pub live_allocs: usize,
    /// Number of heap blocks freed by `velt_rt_str_drop`.
    pub frees: usize,
}

const STACK_BASE: u64 = 1 << 40;
/// Function "addresses": `FUNC_BASE + FuncId` (never dereferenced).
const FUNC_BASE: u64 = 1 << 50;
/// Extern function "addresses": `EXTERN_BASE + ExternId` (never dereferenced).
const EXTERN_BASE: u64 = 1 << 51;
const STEP_LIMIT: u64 = 200_000_000;

/// A value: raw scalar bits or the bytes of an aggregate.
#[derive(Clone, Debug)]
enum V {
    S(u64),
    A(Vec<u8>),
}

impl V {
    fn s(&self) -> u64 {
        match self {
            V::S(v) => *v,
            V::A(_) => panic!("interp: expected scalar"),
        }
    }
}

struct Interp<'p> {
    p: &'p Program,
    heap: Vec<u8>,
    stack: Vec<u8>,
    statics: Vec<u64>,
    allocs: HashMap<u64, u64>,
    stdout: String,
    stderr: String,
    steps: u64,
    frees: usize,
    /// `velt_rt_set_throw_loc`'s slot (address of a static string, 0 = none).
    throw_loc: u64,
    /// Emulated async runtime (async_rt.rs, net.rs).
    exec: async_rt::Exec,
    /// Bounded drop nesting (drops.rs).
    drops: drops::Drops,
}

/// Run `velt_main` to completion (or until panic/exit).
pub(super) fn run(p: &Program) -> Outcome {
    run_in(Interp::new(p)).0
}

/// Run with an emulated http server that receives `requests` (`(method, path, body)`) once
/// the program serves; also returns the `(status, body)` responses in request order.
pub(super) fn run_http(
    p: &Program,
    requests: &[(&str, &str, &str)],
) -> (Outcome, Vec<(u32, String)>) {
    let mut it = Interp::new(p);
    it.exec.http.requests = requests
        .iter()
        .map(|(m, p, b)| (m.to_string(), p.to_string(), b.to_string()))
        .collect();
    run_in(it)
}

fn run_in(mut it: Interp) -> (Outcome, Vec<(u32, String)>) {
    let p = it.p;
    let main = p
        .funcs
        .iter()
        .position(|f| f.symbol == "velt_main")
        .expect("no velt_main");
    let (code, normal) = match it.call_fn(main, vec![]) {
        Ok(v) => (v.s() as u32 as i32, true),
        Err(c) => (c, false),
    };
    let out = Outcome {
        stdout: it.stdout,
        stderr: it.stderr,
        code,
        live_allocs: if normal { it.allocs.len() } else { 0 },
        frees: it.frees,
    };
    (out, it.exec.http.responses)
}

impl<'p> Interp<'p> {
    /// An interpreter with the program's statics loaded.
    fn new(p: &'p Program) -> Self {
        let mut it = Interp {
            p,
            heap: vec![0; 16],
            stack: vec![],
            statics: vec![],
            allocs: HashMap::new(),
            stdout: String::new(),
            stderr: String::new(),
            steps: 0,
            frees: 0,
            throw_loc: 0,
            exec: async_rt::Exec::default(),
            drops: drops::Drops::default(),
        };
        for s in &p.statics {
            let a = it.raw_alloc(s.bytes.len().max(1) as u64);
            it.write_bytes(a, &s.bytes);
            it.statics.push(a);
        }
        // Patched after all statics exist, since a reloc may point at a later static.
        for (s, &base) in p.statics.iter().zip(&it.statics.clone()) {
            for (off, target) in &s.relocs {
                let addr = it.const_addr(target);
                it.write_bytes(base + *off as u64, &addr.to_le_bytes());
            }
        }
        it
    }
}

impl Interp<'_> {
    fn raw_alloc(&mut self, size: u64) -> u64 {
        self.heap.resize(self.heap.len().next_multiple_of(8), 0);
        let a = self.heap.len() as u64;
        self.heap.resize(self.heap.len() + size as usize, 0);
        a
    }

    fn heap_alloc(&mut self, size: u64) -> u64 {
        let a = self.raw_alloc(size.max(1));
        self.allocs.insert(a, size.max(1));
        a
    }

    fn heap_free(&mut self, a: u64) {
        let size = self
            .allocs
            .remove(&a)
            .unwrap_or_else(|| panic!("interp: invalid or double free of {a:#x}"));
        self.heap[a as usize..(a + size) as usize].fill(0xDD);
        self.frees += 1;
    }

    fn mem(&mut self, a: u64, n: usize) -> &mut [u8] {
        assert!(
            a < FUNC_BASE,
            "interp: memory access through a function pointer"
        );
        if a >= STACK_BASE {
            let o = (a - STACK_BASE) as usize;
            &mut self.stack[o..o + n]
        } else {
            assert!(a != 0, "interp: null pointer access");
            &mut self.heap[a as usize..a as usize + n]
        }
    }

    fn read_bytes(&mut self, a: u64, n: usize) -> Vec<u8> {
        self.mem(a, n).to_vec()
    }

    fn write_bytes(&mut self, a: u64, b: &[u8]) {
        self.mem(a, b.len()).copy_from_slice(b);
    }

    fn read_u64(&mut self, a: u64) -> u64 {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(self.mem(a, 8));
        u64::from_le_bytes(buf)
    }

    fn size(&self, t: Ty) -> usize {
        self.p.size_align(t).0 as usize
    }

    fn read(&mut self, a: u64, t: Ty) -> V {
        let n = self.size(t);
        let b = self.read_bytes(a, n);
        match t {
            Ty::Agg(_) => V::A(b),
            _ => {
                let mut buf = [0u8; 8];
                buf[..n].copy_from_slice(&b);
                V::S(u64::from_le_bytes(buf))
            }
        }
    }

    fn write(&mut self, a: u64, t: Ty, v: &V) {
        let n = self.size(t);
        match v {
            V::A(b) => self.write_bytes(a, &b[..n]),
            V::S(x) => self.write_bytes(a, &x.to_le_bytes()[..n]),
        }
    }

    /// Execute a function; `Err(code)` propagates a process exit (panic / `process.exit`).
    fn call_fn(&mut self, fi: usize, args: Vec<V>) -> Result<V, i32> {
        let f = &self.p.funcs[fi];
        let frame_start = self.stack.len();
        let mut slots = vec![];
        for l in &f.locals {
            self.stack.resize(self.stack.len().next_multiple_of(8), 0);
            slots.push(STACK_BASE + self.stack.len() as u64);
            // Poison uninitialized memory so reads of garbage show up in outputs.
            let n = self.size(l.ty).max(1);
            self.stack.resize(self.stack.len() + n, 0xCC);
        }
        for (i, a) in args.iter().enumerate() {
            self.write(slots[i], f.params[i], a);
        }
        let mut bb = 0usize;
        let result = loop {
            self.steps += 1;
            assert!(self.steps < STEP_LIMIT, "interp: step limit");
            for s in &f.blocks[bb].stmts {
                self.exec_stmt(f, &slots, s);
            }
            match self.exec_term(f, &slots, &f.blocks[bb].term)? {
                Ok(next) => bb = next,
                Err(ret) => break ret,
            }
        };
        self.stack.truncate(frame_start);
        Ok(result)
    }

    fn exec_stmt(&mut self, f: &Function, slots: &[u64], s: &Stmt) {
        match s {
            Stmt::Assign(pl, rv) => {
                let (addr, ty) = self.place(f, slots, pl);
                let v = self.rvalue(f, slots, rv);
                self.write(addr, ty, &v);
            }
            Stmt::MemCopy { dst, src, size } => {
                let d = self.operand(f, slots, dst).0.s();
                let s = self.operand(f, slots, src).0.s();
                let b = self.read_bytes(s, *size as usize);
                self.write_bytes(d, &b);
            }
            Stmt::MemCopyDyn { dst, src, len, .. } => {
                let d = self.operand(f, slots, dst).0.s();
                let s = self.operand(f, slots, src).0.s();
                let n = self.operand(f, slots, len).0.s() as usize;
                // Reading the whole source before writing gives memmove semantics for both forms.
                let b = self.read_bytes(s, n);
                self.write_bytes(d, &b);
            }
            Stmt::MemSet { dst, byte, len } => {
                let d = self.operand(f, slots, dst).0.s();
                let b = self.operand(f, slots, byte).0.s() as u8;
                let n = self.operand(f, slots, len).0.s() as usize;
                self.mem(d, n).fill(b);
            }
            Stmt::Nop => {}
        }
    }

    /// Encoded address of a `Static`/`Func`/`Extern` constant.
    fn const_addr(&self, c: &Const) -> u64 {
        match c {
            Const::Static(s) => self.statics[s.0 as usize],
            Const::Func(id) => FUNC_BASE + id.0 as u64,
            Const::Extern(id) => EXTERN_BASE + id.0 as u64,
            c => panic!("interp: {c:?} has no address"),
        }
    }

    /// `Ok(Ok(next block))`, `Ok(Err(return value))`, or `Err(exit code)`.
    fn exec_term(
        &mut self,
        f: &Function,
        slots: &[u64],
        t: &Terminator,
    ) -> Result<Result<usize, V>, i32> {
        Ok(match t {
            Terminator::Goto(b) => Ok(b.0 as usize),
            Terminator::Branch { cond, then, els } => {
                let c = self.operand(f, slots, cond).0.s() & 1 == 1;
                Ok(if c { then.0 as usize } else { els.0 as usize })
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let (v, t) = self.operand(f, slots, value);
                let v = to_i128(v.s(), t);
                Ok(cases
                    .iter()
                    .find(|c| c.0 == v)
                    .map_or(default.0 as usize, |c| c.1 .0 as usize))
            }
            Terminator::Return(o) => Err(self.operand(f, slots, o).0),
            Terminator::Unreachable => panic!("interp: reached unreachable in {}", f.symbol),
            Terminator::Call {
                callee,
                args,
                dest,
                next,
            } => {
                let argv: Vec<V> = args.iter().map(|a| self.operand(f, slots, a).0).collect();
                let r = match callee {
                    Callee::Func(id) => self.call_fn(id.0 as usize, argv)?,
                    Callee::Extern(id) => {
                        let raw: Vec<u64> = argv.iter().map(V::s).collect();
                        V::S(self.rt(&self.p.externs[id.0 as usize].symbol, &raw)?)
                    }
                    Callee::Ptr {
                        target,
                        params,
                        ret,
                    } => {
                        let t = self.operand(f, slots, target).0.s();
                        if let Some(e) = t.checked_sub(EXTERN_BASE) {
                            let raw: Vec<u64> = argv.iter().map(V::s).collect();
                            let sym = &self.p.externs[e as usize].symbol;
                            V::S(self.rt(sym, &raw)?)
                        } else {
                            let id = t
                                .checked_sub(FUNC_BASE)
                                .expect("interp: call through a non-function pointer");
                            let callee = &self.p.funcs[id as usize];
                            assert!(
                                &callee.params == params && callee.ret == *ret,
                                "interp: indirect call signature mismatch calling {}",
                                callee.symbol
                            );
                            self.call_fn(id as usize, argv)?
                        }
                    }
                };
                if let Some(d) = dest {
                    let (addr, ty) = self.place(f, slots, d);
                    self.write(addr, ty, &r);
                }
                Ok(next.0 as usize)
            }
        })
    }

    fn place(&mut self, f: &Function, slots: &[u64], p: &Place) -> (u64, Ty) {
        let mut addr = slots[p.local.0 as usize];
        let mut t = f.locals[p.local.0 as usize].ty;
        for pr in &p.proj {
            match (pr, t) {
                (Proj::Field(n), Ty::Agg(a)) => {
                    let (ft, off) = self.p.aggs[a.0 as usize].fields[*n as usize];
                    addr += off as u64;
                    t = ft;
                }
                (Proj::Deref(to), Ty::Ptr) => {
                    addr = self.read_u64(addr);
                    t = *to;
                }
                (Proj::Cast(a), _) => t = Ty::Agg(*a),
                _ => panic!("interp: bad projection"),
            }
        }
        (addr, t)
    }

    fn operand(&mut self, f: &Function, slots: &[u64], o: &Operand) -> (V, Ty) {
        match o {
            Operand::Copy(p) => {
                let (a, t) = self.place(f, slots, p);
                (self.read(a, t), t)
            }
            Operand::Const(c, t) => {
                let v = match c {
                    Const::Int(v) if *t == Ty::Ptr || *t == Ty::Bool => *v as u64,
                    Const::Int(v) => from_i128(*v, *t),
                    Const::Float(x) => from_f(*x, *t),
                    Const::Bool(b) => *b as u64,
                    Const::Unit => 0,
                    c => self.const_addr(c),
                };
                (V::S(v), *t)
            }
        }
    }

    fn rvalue(&mut self, f: &Function, slots: &[u64], rv: &Rvalue) -> V {
        match rv {
            Rvalue::Use(o) => self.operand(f, slots, o).0,
            Rvalue::Unary(op, o) => {
                let (v, t) = self.operand(f, slots, o);
                let v = v.s();
                V::S(match op {
                    UnOp::Neg if t.is_float() => from_f(-to_f(v, t), t),
                    UnOp::Neg => from_i128(to_i128(v, t).wrapping_neg(), t),
                    UnOp::Not => (v & 1) ^ 1,
                    UnOp::BitNot => !v & mask(arith::bits_of(t)),
                })
            }
            Rvalue::Binary(op, a, b) => {
                let (a, t) = self.operand(f, slots, a);
                let (b, tb) = self.operand(f, slots, b);
                V::S(binop(*op, a.s(), b.s(), t, tb))
            }
            Rvalue::Cast(o, to) => {
                let (v, from) = self.operand(f, slots, o);
                V::S(cast(v.s(), from, *to))
            }
            Rvalue::AddrOf(p) => V::S(self.place(f, slots, p).0),
            Rvalue::Aggregate(a, ops) => self.aggregate(f, slots, *a, ops),
        }
    }

    fn aggregate(&mut self, f: &Function, slots: &[u64], a: AggId, ops: &[Operand]) -> V {
        let layout = &self.p.aggs[a.0 as usize];
        let mut buf = vec![0u8; layout.size as usize];
        for (o, (ft, off)) in ops.iter().zip(&layout.fields) {
            let n = self.size(*ft);
            let bytes = match self.operand(f, slots, o).0 {
                V::A(b) => b,
                V::S(x) => x.to_le_bytes().to_vec(),
            };
            buf[*off as usize..*off as usize + n].copy_from_slice(&bytes[..n]);
        }
        V::A(buf)
    }
}
