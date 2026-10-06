//! Unit tests for scalar replacement of heap objects on hand-built VIR: each program is run in
//! the interpreter (with a heap) before and after the pass.

use super::*;
use crate::interp::{Interp, Memory, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::Ty::*;
use velt_vir::vir::{
    AggId, BinOp, BlockId, Callee, ExternFn, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator,
};

/// A host with a heap that counts live objects and records the other extern calls.
#[derive(Default)]
struct HeapHost {
    live: i64,
    allocs: u64,
    calls: Vec<String>,
}

impl crate::interp::Host for HeapHost {
    fn call(&mut self, ext: &ExternFn, args: &[u64], mem: &mut Memory) -> Result<u64, Trap> {
        match ext.symbol.as_str() {
            "velt_rt_alloc" => {
                self.live += 1;
                self.allocs += 1;
                mem.alloc_heap(args[0], args[1])
            }
            "velt_rt_free" => {
                self.live -= 1;
                Ok(0)
            }
            other => {
                self.calls.push(format!("{other}{:?}", &args[1..]));
                Ok(0)
            }
        }
    }
}

/// (result, objects allocated, objects leaked, other extern calls)
fn run_main(p: &Program, args: &[u64]) -> (u64, u64, i64, Vec<String>) {
    let mut interp = Interp::new(p, HeapHost::default());
    let r = interp.call_symbol("main", args).expect("program runs");
    let h = interp.host;
    (r, h.allocs, h.live, h.calls)
}

/// Builder state: the allocator externs, an opaque `keep(ptr)` call and the object layout.
struct Env {
    pb: ProgramBuilder,
    alloc: ExternId,
    free: ExternId,
    keep: ExternId,
    /// `make() -> ptr`: an opaque call returning a pointer.
    make: ExternId,
    obj: AggId,
    /// Another aggregate of the object's size.
    other: AggId,
}

fn env() -> Env {
    let mut pb = ProgramBuilder::new();
    let alloc = pb.ext("velt_rt_alloc", &[U64, U64], Ptr, false);
    let free = pb.ext("velt_rt_free", &[Ptr, U64, U64], Unit, false);
    let keep = pb.ext("keep", &[Ptr, I64], Unit, false);
    let make = pb.ext("make", &[], Ptr, false);
    let obj = pb.agg("Pair object", 16, 8, &[(I64, 0), (I64, 8)]);
    let other = pb.agg("Other object", 16, 8, &[(I64, 0), (I64, 8)]);
    Env {
        pb,
        alloc,
        free,
        keep,
        make,
        obj,
        other,
    }
}

impl Env {
    fn field(&self, w: Local, k: u32) -> Place {
        Place {
            local: w,
            proj: vec![Proj::Deref(Ty::Agg(self.obj)), Proj::Field(k)],
        }
    }

    /// `w = new Pair(x, y)` as lowering emits it; returns the continuation.
    fn new_obj(
        &self,
        fb: &mut FuncBuilder,
        b: BlockId,
        w: Local,
        x: Operand,
        y: Operand,
    ) -> BlockId {
        let args = vec![int(16, U64), int(8, U64)];
        let b = fb.call(b, Callee::Extern(self.alloc), args, Some(w));
        let len = int(16, U64);
        fb.push(
            b,
            Stmt::MemSet {
                dst: copy_local(w),
                byte: int(0, U8),
                len,
            },
        );
        fb.push(b, Stmt::Assign(self.field(w, 0), Rvalue::Use(x)));
        fb.push(b, Stmt::Assign(self.field(w, 1), Rvalue::Use(y)));
        b
    }

    /// The drop of `w`: `t = w; if (w != null) free(t)`; returns the continuation.
    fn drop_obj(&self, fb: &mut FuncBuilder, b: BlockId, w: Local) -> BlockId {
        let (t, c) = (fb.local(Ptr), fb.local(Bool));
        fb.assign(b, t, Rvalue::Use(copy_local(w)));
        fb.assign(b, c, bin(BinOp::Ne, copy_local(w), int(0, Ptr)));
        let (free, next) = (fb.block(), fb.block());
        fb.branch(b, c, free, next);
        let args = vec![copy_local(t), int(16, U64), int(8, U64)];
        let after = fb.call(free, Callee::Extern(self.free), args, None);
        fb.goto(after, next);
        next
    }

    /// `r = w.0 + w.1`
    fn sum(&self, fb: &mut FuncBuilder, b: BlockId, w: Local) -> Local {
        let r = fb.local(I64);
        let (x, y) = (copy_place(self.field(w, 0)), copy_place(self.field(w, 1)));
        fb.assign(b, r, bin(BinOp::Add, x, y));
        r
    }
}

/// Run the pass on `main` (the last function); check validity, behaviour on `inputs`, and
/// whether it replaced anything (then no allocation may remain).
fn check(original: Program, inputs: &[u64], expect_replaced: bool) -> Program {
    let mut p = original.clone();
    let aggs = p.aggs.clone();
    let allocator = Allocator::find(&p);
    let last = p.funcs.len() - 1;
    let changed = run(&aggs, allocator, &mut p.funcs[last]);
    assert_eq!(changed, expect_replaced, "{p}");
    assert_valid(&p);
    for &n in inputs {
        let (r0, _, leak0, calls0) = run_main(&original, &[n]);
        let (r1, allocs1, leak1, calls1) = run_main(&p, &[n]);
        assert_eq!((r0, leak0, calls0), (r1, leak1, calls1), "n = {n}\n{p}");
        if expect_replaced {
            assert_eq!(allocs1, 0, "{p}");
        }
    }
    p
}

/// `p = new(0, 1); for i < n { t = new(p.0 + p.1, p.1 + 1); drop p; p = t } return p.0 + p.1`
/// (the loop-carried immutable object of `p = p.add(v)`).
fn loop_program(
    between: impl Fn(&Env, &mut FuncBuilder, BlockId, Local, Local) -> BlockId,
) -> Program {
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let n = fb.param(0);
    let (p, t, i, c) = (fb.local(Ptr), fb.local(Ptr), fb.local(I64), fb.local(Bool));
    let b = fb.block();
    fb.assign(b, p, Rvalue::Use(int(0, Ptr)));
    fb.assign(b, i, Rvalue::Use(int(0, I64)));
    let b = env.new_obj(&mut fb, b, p, int(0, I64), int(1, I64));
    let head = fb.block();
    fb.goto(b, head);
    let (body, exit) = (fb.block(), fb.block());
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    fb.branch(head, c, body, exit);
    let (x, y) = (fb.local(I64), fb.local(I64));
    fb.assign(
        body,
        x,
        bin(
            BinOp::Add,
            copy_place(env.field(p, 0)),
            copy_place(env.field(p, 1)),
        ),
    );
    fb.assign(
        body,
        y,
        bin(BinOp::Add, copy_place(env.field(p, 1)), int(1, I64)),
    );
    let b = env.new_obj(&mut fb, body, t, copy_local(x), copy_local(y));
    let b = between(&env, &mut fb, b, p, t);
    let b = env.drop_obj(&mut fb, b, p);
    fb.assign(b, p, Rvalue::Use(copy_local(t)));
    fb.assign(b, i, bin(BinOp::Add, copy_local(i), int(1, I64)));
    fb.goto(b, head);
    let r = env.sum(&mut fb, exit, p);
    let b = env.drop_obj(&mut fb, exit, p);
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    env.pb.finish()
}

#[test]
fn replaces_a_loop_carried_object() {
    let p = check(loop_program(|_, _, b, _, _| b), &[0, 1, 7], true);
    let mains = &p.funcs[0];
    let allocator = Allocator::find(&p).unwrap();
    let calls = mains.blocks.iter().filter(|b| {
        matches!(&b.term, Terminator::Call { callee: Callee::Extern(e), .. }
            if *e == allocator.alloc || *e == allocator.free)
    });
    assert_eq!(calls.count(), 0);
}

#[test]
fn a_borrowing_alias_that_only_reads_is_fine() {
    // `this = t; keep(null, this.0)` between: `this` is read while `t` is still live.
    let p = loop_program(|env, fb, b, _, t| {
        let this = fb.local(Ptr);
        fb.assign(b, this, Rvalue::Use(copy_local(t)));
        let r = env.sum(fb, b, this);
        let args = vec![int(0, Ptr), copy_local(r)];
        fb.call(b, Callee::Extern(env.keep), args, None)
    });
    check(p, &[0, 3], true);
}

#[test]
fn a_write_seen_through_another_local_keeps_the_heap() {
    // `this = t; this.0 = 100;` then `t` is read: the write must be visible through `t`.
    let p = loop_program(|env, fb, b, _, t| {
        let this = fb.local(Ptr);
        fb.assign(b, this, Rvalue::Use(copy_local(t)));
        fb.push(
            b,
            Stmt::Assign(env.field(this, 0), Rvalue::Use(int(100, I64))),
        );
        b
    });
    check(p, &[0, 3], false);
}

#[test]
fn a_write_after_the_last_read_of_the_alias_is_fine() {
    // `this = p` (the old object, dead after the drop below), written: nothing reads it.
    let p = loop_program(|env, fb, b, p, _| {
        let this = fb.local(Ptr);
        fb.assign(b, this, Rvalue::Use(copy_local(p)));
        fb.push(
            b,
            Stmt::Assign(env.field(this, 0), Rvalue::Use(int(100, I64))),
        );
        b
    });
    check(p, &[0, 3], true);
}

#[test]
fn escaping_pointers_keep_the_heap() {
    // Passed to a call.
    let passed = loop_program(|env, fb, b, _, t| {
        let args = vec![copy_local(t), int(1, I64)];
        fb.call(b, Callee::Extern(env.keep), args, None)
    });
    check(passed, &[2], false);
    // Compared with another object (identity).
    let compared = loop_program(|_, fb, b, p, t| {
        let c = fb.local(Bool);
        fb.assign(b, c, bin(BinOp::Eq, copy_local(p), copy_local(t)));
        b
    });
    check(compared, &[2], false);
    // Its address stored in memory (here: in another object).
    let stored = loop_program(|env, fb, b, p, t| {
        let place = Place {
            local: p,
            proj: vec![Proj::Deref(Ty::Agg(env.obj)), Proj::Field(0)],
        };
        let cast = fb.local(I64);
        fb.assign(b, cast, Rvalue::Cast(copy_local(t), I64));
        fb.push(b, Stmt::Assign(place, Rvalue::Use(copy_local(cast))));
        b
    });
    check(stored, &[2], false);
}

#[test]
fn returned_objects_keep_the_heap() {
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[I64], Ptr);
    let w = fb.local(Ptr);
    let b = fb.block();
    let x = copy_local(fb.param(0));
    let b = env.new_obj(&mut fb, b, w, x, int(2, I64));
    fb.ret(b, copy_local(w));
    env.pb.add(fb.finish());
    let mut p = env.pb.finish();
    let aggs = p.aggs.clone();
    let allocator = Allocator::find(&p);
    assert!(!run(&aggs, allocator, &mut p.funcs[0]));
}

#[test]
fn objects_seen_as_another_type_keep_the_heap() {
    let p = loop_program(|_, fb, b, _, t| {
        let x = fb.local(I64);
        fb.assign(b, x, Rvalue::Use(copy_place(deref(t, I64))));
        b
    });
    check(p, &[2], false);
}

/// `loop_program` with `between` must keep its objects on the heap. The inputs run the loop
/// body zero times (a few of these programs are not meant to run: an indirect call to an
/// object), so only the decision and the unchanged program are checked.
fn stays_on_heap(between: impl Fn(&Env, &mut FuncBuilder, BlockId, Local, Local) -> BlockId) {
    check(loop_program(between), &[0], false);
}

#[test]
fn the_address_of_a_field_keeps_the_heap() {
    stays_on_heap(|env, fb, b, _, t| {
        let a = fb.local(Ptr);
        fb.assign(b, a, Rvalue::AddrOf(env.field(t, 0)));
        b
    });
}

#[test]
fn an_offset_pointer_keeps_the_heap() {
    // A reference-counted object's header sits in front of it.
    stays_on_heap(|_, fb, b, _, t| {
        let h = fb.local(Ptr);
        fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(t), int(-8, I64)));
        b
    });
}

#[test]
fn memory_copies_keep_the_heap() {
    for into_object in [true, false] {
        stays_on_heap(|env, fb, b, _, t| {
            let (buf, q) = (fb.local(Ty::Agg(env.obj)), fb.local(Ptr));
            fb.assign(b, q, Rvalue::AddrOf(Place::local(buf)));
            let (dst, src) = if into_object { (t, q) } else { (q, t) };
            let copy = Stmt::MemCopy {
                dst: copy_local(dst),
                src: copy_local(src),
                size: 16,
            };
            fb.push(b, copy);
            b
        });
    }
}

#[test]
fn an_indirect_call_through_the_object_keeps_the_heap() {
    stays_on_heap(|_, fb, b, _, t| {
        let callee = Callee::Ptr {
            target: copy_local(t),
            params: vec![],
            ret: Unit,
        };
        fb.call(b, callee, vec![], None)
    });
}

#[test]
fn other_fills_keep_the_heap() {
    for (byte, len) in [(1, 16), (0, 8)] {
        stays_on_heap(|_, fb, b, _, t| {
            let fill = Stmt::MemSet {
                dst: copy_local(t),
                byte: int(byte, U8),
                len: int(len, U64),
            };
            fb.push(b, fill);
            b
        });
    }
}

#[test]
fn blocks_of_another_size_keep_the_heap() {
    // `t` reallocated with a size or alignment that is not the object's.
    for (size, align) in [(24, 8), (16, 16)] {
        stays_on_heap(|env, fb, b, _, t| {
            let args = vec![int(size, U64), int(align, U64)];
            fb.call(b, Callee::Extern(env.alloc), args, Some(t))
        });
    }
    // `p` freed with a size or alignment that is not the object's.
    for (size, align) in [(8, 8), (16, 4)] {
        stays_on_heap(|env, fb, b, p, _| {
            let args = vec![copy_local(p), int(size, U64), int(align, U64)];
            fb.call(b, Callee::Extern(env.free), args, None)
        });
    }
}

#[test]
fn an_access_as_another_aggregate_keeps_the_heap() {
    stays_on_heap(|env, fb, b, _, t| {
        let x = fb.local(I64);
        let place = Place {
            local: t,
            proj: vec![Proj::Deref(Ty::Agg(env.other)), Proj::Field(0)],
        };
        fb.assign(b, x, Rvalue::Use(copy_place(place)));
        b
    });
}

#[test]
fn a_pointer_returned_by_a_call_keeps_the_heap() {
    stays_on_heap(|env, fb, b, _, t| fb.call(b, Callee::Extern(env.make), vec![], Some(t)));
}
