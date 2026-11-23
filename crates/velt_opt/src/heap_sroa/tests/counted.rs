//! Counted objects (#1718): the count in front of the object, and the soundness cases: what
//! keeps a counted object on the heap
//! (a weak reference, the cycle collector) and what replacing one must keep (its own counted
//! fields' releases, cleanup pads, a drop with side effects, a drop through a known vtable).

use super::*;
use velt_vir::vir::{Const, StaticId};

impl Env {
    /// `w = new Pair(x, y)` for a counted object: a count of 1 in the 8 bytes before it.
    fn new_counted(
        &self,
        fb: &mut FuncBuilder,
        b: BlockId,
        w: Local,
        x: Operand,
        y: Operand,
    ) -> BlockId {
        let base = fb.local(Ptr);
        let args = vec![int(24, U64), int(8, U64)];
        let b = fb.call(b, Callee::Extern(self.alloc), args, Some(base));
        fb.assign(b, base, Rvalue::Use(copy_local(base)));
        fb.push(b, Stmt::Assign(deref(base, U64), Rvalue::Use(int(1, U64))));
        fb.assign(b, w, bin(BinOp::PtrAdd, copy_local(base), int(8, I64)));
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

    /// `w`'s count, plus `by` (a retain is `by = 1`).
    fn bump(&self, fb: &mut FuncBuilder, b: BlockId, w: Local, by: i128) {
        let (h, c) = (fb.local(Ptr), fb.local(U64));
        fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(w), int(-8, I64)));
        fb.assign(b, c, Rvalue::Use(copy_place(deref(h, U64))));
        let (op, by) = if by < 0 {
            (BinOp::Sub, -by)
        } else {
            (BinOp::Add, by)
        };
        fb.push(
            b,
            Stmt::Assign(deref(h, U64), bin(op, copy_local(c), int(by, U64))),
        );
    }

    /// The release of `w`: `if (w != null) { if (count == 1) free else count -= 1 }`.
    fn release(&self, fb: &mut FuncBuilder, b: BlockId, w: Local, size: i128) -> BlockId {
        let (h, c, n, one) = (fb.local(Ptr), fb.local(Bool), fb.local(U64), fb.local(Bool));
        fb.assign(b, c, bin(BinOp::Ne, copy_local(w), int(0, Ptr)));
        let (some, next) = (fb.block(), fb.block());
        fb.branch(b, c, some, next);
        fb.assign(some, h, bin(BinOp::PtrAdd, copy_local(w), int(-8, I64)));
        fb.assign(some, n, Rvalue::Use(copy_place(deref(h, U64))));
        fb.assign(some, one, bin(BinOp::Eq, copy_local(n), int(1, U64)));
        let (last, shared) = (fb.block(), fb.block());
        fb.branch(some, one, last, shared);
        let args = vec![copy_local(h), int(size, U64), int(8, U64)];
        let after = fb.call(last, Callee::Extern(self.free), args, None);
        fb.goto(after, next);
        let dec = bin(BinOp::Sub, copy_local(n), int(1, U64));
        fb.push(shared, Stmt::Assign(deref(h, U64), dec));
        fb.goto(shared, next);
        next
    }
}

/// `loop_program` with counted objects: each round also takes a second reference `q` to the
/// new object (a retain) and lets it go (a release), and the result adds the last object's
/// count, so a count the rewrite got wrong changes the result or leaks.
fn counted_program(
    size: i128,
    between: impl Fn(&Env, &mut FuncBuilder, BlockId, Local, Local) -> BlockId,
) -> Program {
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let n = fb.param(0);
    let (p, t, q, i, c) = (
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(I64),
        fb.local(Bool),
    );
    let b = fb.block();
    fb.assign(b, p, Rvalue::Use(int(0, Ptr)));
    fb.assign(b, q, Rvalue::Use(int(0, Ptr)));
    fb.assign(b, i, Rvalue::Use(int(0, I64)));
    let b = env.new_counted(&mut fb, b, p, int(0, I64), int(1, I64));
    let head = fb.block();
    fb.goto(b, head);
    let (body, exit) = (fb.block(), fb.block());
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    fb.branch(head, c, body, exit);
    let x = env.sum(&mut fb, body, p);
    let b = env.new_counted(&mut fb, body, t, copy_local(x), int(1, I64));
    fb.assign(b, q, Rvalue::Use(copy_local(t)));
    env.bump(&mut fb, b, q, 1);
    let b = between(&env, &mut fb, b, p, t);
    let b = env.release(&mut fb, b, q, size);
    let b = env.release(&mut fb, b, p, size);
    fb.assign(b, p, Rvalue::Use(copy_local(t)));
    fb.assign(b, i, bin(BinOp::Add, copy_local(i), int(1, I64)));
    fb.goto(b, head);
    let r = env.sum(&mut fb, exit, p);
    let (h, k, kk, out) = (fb.local(Ptr), fb.local(U64), fb.local(I64), fb.local(I64));
    fb.assign(exit, h, bin(BinOp::PtrAdd, copy_local(p), int(-8, I64)));
    fb.assign(exit, k, Rvalue::Use(copy_place(deref(h, U64))));
    fb.assign(exit, kk, Rvalue::Cast(copy_local(k), I64));
    fb.assign(exit, out, bin(BinOp::Add, copy_local(r), copy_local(kk)));
    let b = env.release(&mut fb, exit, p, size);
    fb.ret(b, copy_local(out));
    env.pb.add(fb.finish());
    env.pb.finish()
}

#[test]
fn replaces_a_counted_object() {
    let p = counted_program(24, |_, _, b, _, _| b);
    check(p, &[0, 1, 4], true);
}

#[test]
fn a_counted_object_with_a_second_reference_alive_keeps_its_count() {
    // `q` keeps the new object while the old one is let go: its count is 2 until `q`'s release.
    let p = counted_program(24, |env, fb, b, _, t| {
        env.bump(fb, b, t, 1);
        env.bump(fb, b, t, -1);
        b
    });
    check(p, &[0, 3], true);
}

#[test]
fn a_counted_block_of_another_size_keeps_the_heap() {
    // Freed as 16 bytes: the count is not in front of a 16-byte object then.
    check(counted_program(16, |_, _, b, _, _| b), &[0], false);
}

#[test]
fn a_count_whose_address_escapes_keeps_the_heap() {
    let p = counted_program(24, |env, fb, b, _, t| {
        let h = fb.local(Ptr);
        fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(t), int(-8, I64)));
        fb.call(
            b,
            Callee::Extern(env.keep),
            vec![copy_local(h), int(0, I64)],
            None,
        )
    });
    check(p, &[0], false);
}

#[test]
fn a_count_read_as_another_type_keeps_the_heap() {
    let p = counted_program(24, |_, fb, b, _, t| {
        let (h, x) = (fb.local(Ptr), fb.local(U32));
        fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(t), int(-8, I64)));
        fb.assign(b, x, Rvalue::Use(copy_place(deref(h, U32))));
        b
    });
    check(p, &[0], false);
}

#[test]
fn a_pointer_into_the_middle_of_a_counted_object_keeps_the_heap() {
    let p = counted_program(24, |_, fb, b, _, t| {
        let (h, x) = (fb.local(Ptr), fb.local(I64));
        fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(t), int(8, I64)));
        fb.assign(b, x, Rvalue::Use(copy_place(deref(h, I64))));
        b
    });
    check(p, &[0], false);
}

#[test]
fn a_counted_object_copied_in_a_loop_it_is_not_made_in_keeps_the_heap() {
    // `p = new(1, 2); for i < n { q = p; s += q.0 } release p`: in locals, each round would copy
    // the whole object where the heap copies a pointer (`cost`).
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let n = fb.param(0);
    let (p, q, i, s, c) = (
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(I64),
        fb.local(I64),
        fb.local(Bool),
    );
    let b = fb.block();
    fb.assign(b, p, Rvalue::Use(int(0, Ptr)));
    fb.assign(b, q, Rvalue::Use(int(0, Ptr)));
    fb.assign(b, i, Rvalue::Use(int(0, I64)));
    fb.assign(b, s, Rvalue::Use(int(0, I64)));
    let b = env.new_counted(&mut fb, b, p, int(1, I64), int(2, I64));
    let head = fb.block();
    fb.goto(b, head);
    let (body, exit) = (fb.block(), fb.block());
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    fb.branch(head, c, body, exit);
    fb.assign(body, q, Rvalue::Use(copy_local(p)));
    let x = fb.local(I64);
    fb.assign(body, x, Rvalue::Use(copy_place(env.field(q, 0))));
    fb.assign(body, s, bin(BinOp::Add, copy_local(s), copy_local(x)));
    fb.assign(body, i, bin(BinOp::Add, copy_local(i), int(1, I64)));
    fb.goto(body, head);
    let b = env.release(&mut fb, exit, p, 24);
    fb.ret(b, copy_local(s));
    env.pb.add(fb.finish());
    check(env.pb.finish(), &[0, 3], false);
}

/// The weak-capable release of `w` (lowering's `release_weak`): `c as i64 > 1` decrements,
/// `c == 1` frees, anything else (a weakly held object, `RC_WEAK` in the sign bit, or one the
/// cycle collector buffered) calls `hook(w as u64)`, which answers whether this was the last
/// reference.
fn release_hooked(
    env: &Env,
    fb: &mut FuncBuilder,
    b: BlockId,
    w: Local,
    hook: ExternId,
) -> BlockId {
    let (h, n, s, shared) = (fb.local(Ptr), fb.local(U64), fb.local(I64), fb.local(Bool));
    fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(w), int(-8, I64)));
    fb.assign(b, n, Rvalue::Use(copy_place(deref(h, U64))));
    fb.assign(b, s, Rvalue::Cast(copy_local(n), I64));
    fb.assign(b, shared, bin(BinOp::Gt, copy_local(s), int(1, I64)));
    let (dec, other, join) = (fb.block(), fb.block(), fb.block());
    fb.branch(b, shared, dec, other);
    let m = bin(BinOp::Sub, copy_local(n), int(1, U64));
    fb.push(dec, Stmt::Assign(deref(h, U64), m));
    fb.goto(dec, join);
    let one = fb.local(Bool);
    fb.assign(other, one, bin(BinOp::Eq, copy_local(n), int(1, U64)));
    let (last, cold) = (fb.block(), fb.block());
    fb.branch(other, one, last, cold);
    let (x, gone) = (fb.local(U64), fb.local(Bool));
    fb.assign(cold, x, Rvalue::Cast(copy_local(w), U64));
    let after = fb.call(cold, Callee::Extern(hook), vec![copy_local(x)], Some(gone));
    fb.branch(after, gone, last, join);
    let args = vec![copy_local(h), int(24, U64), int(8, U64)];
    let after = fb.call(last, Callee::Extern(env.free), args, None);
    fb.goto(after, join);
    join
}

/// `w = new(n, 2); q = w (a retain); release q; r = w.0 + w.1; release w; return r`, the
/// releases through `hook`.
fn hooked_program(hook: &str) -> Program {
    let mut env = env();
    let hook = env.pb.ext(hook, &[U64], Bool, false);
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let n = copy_local(fb.param(0));
    let (w, q) = (fb.local(Ptr), fb.local(Ptr));
    let b = fb.block();
    let b = env.new_counted(&mut fb, b, w, n, int(2, I64));
    fb.assign(b, q, Rvalue::Use(copy_local(w)));
    env.bump(&mut fb, b, q, 1);
    let b = release_hooked(&env, &mut fb, b, q, hook);
    let r = env.sum(&mut fb, b, w);
    let b = release_hooked(&env, &mut fb, b, w, hook);
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    env.pb.finish()
}

#[test]
fn a_counted_object_a_weak_reference_may_hold_keeps_the_heap() {
    // `weakRelease` (std/lang/hooks/weak.velt) takes the object's address: a weak map or
    // `WeakRef` finds the object by it, so the object must stay where it is.
    check(hooked_program("weakRelease"), &[0, 5], false);
}

#[test]
fn a_counted_object_the_cycle_collector_may_buffer_keeps_the_heap() {
    // The collector's release hook (docs/internals/design/cycles.md "Possible roots") records
    // the object's address in its root set.
    check(hooked_program("cycleRelease"), &[0, 5], false);
}

/// Whether `f` still mentions the heap: a call of `free` or a place read or written through a
/// pointer.
fn uses_the_heap(f: &Function, free: ExternId) -> bool {
    let through = |p: &Place| p.proj.iter().any(|j| matches!(j, Proj::Deref(_)));
    f.blocks.iter().any(|b| {
        let frees =
            matches!(&b.term, Terminator::Call { callee: Callee::Extern(e), .. } if *e == free);
        let mut derefs = false;
        for s in &b.stmts {
            crate::visit::stmt_operands(s, &mut |op| {
                derefs |= matches!(op, Operand::Copy(p) if through(p));
            });
            derefs |= matches!(s, Stmt::Assign(d, _) if through(d));
        }
        frees || derefs
    })
}

#[test]
fn a_cleanup_pad_that_releases_the_object_is_rewritten_with_it() {
    // `w = new(n, 2); x = g() unwind pad; r = w.0 + w.1 + x; release w; return r`, the pad
    // `release w; velt_eh_resume()`: the object is live into the pad, which releases it as the
    // normal path does, so the pad's release goes to the locals too (no free of a block that was
    // never allocated, no read through its pointer).
    let mut env = env();
    let resume = env.pb.ext("velt_eh_resume", &[], Unit, true);
    let mut gb = FuncBuilder::internal("g", &[], I64);
    let gb_entry = gb.block();
    gb.ret(gb_entry, int(7, I64));
    let g = env.pb.add(gb.finish());
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let n = copy_local(fb.param(0));
    let (w, x, r) = (fb.local(Ptr), fb.local(I64), fb.local(I64));
    let b = fb.block();
    let b = env.new_counted(&mut fb, b, w, n, int(2, I64));
    let (body, pad) = (fb.block(), fb.block());
    let call = Terminator::Call {
        callee: Callee::Func(g),
        args: vec![],
        dest: Some(Place::local(x)),
        next: body,
        unwind: Some(pad),
    };
    fb.term(b, call);
    let s = env.sum(&mut fb, body, w);
    fb.assign(body, r, bin(BinOp::Add, copy_local(s), copy_local(x)));
    let b = env.release(&mut fb, body, w, 24);
    fb.ret(b, copy_local(r));
    let b = env.release(&mut fb, pad, w, 24);
    let after = fb.call(b, Callee::Extern(resume), vec![], None);
    fb.term(after, Terminator::Unreachable);
    env.pb.add(fb.finish());
    let free = env.free;
    let original = env.pb.finish();
    assert!(uses_the_heap(original.funcs.last().expect("main"), free));
    let p = check(original, &[0, 3], true);
    let main = p.funcs.last().expect("main");
    assert!(!uses_the_heap(main, free), "{p}");
}

#[test]
fn a_counted_object_whose_drop_has_effects_runs_it_where_its_last_reference_goes() {
    // The drop glue inlined into the release (a `[Symbol.dispose]()` that logs `w.1`): `q = w`
    // (a retain); release q; mark 1; release w (the last: dispose); mark 2. The effect stays
    // between the marks, and runs once.
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let n = copy_local(fb.param(0));
    let (w, q) = (fb.local(Ptr), fb.local(Ptr));
    let b = fb.block();
    let b = env.new_counted(&mut fb, b, w, int(4, I64), n);
    fb.assign(b, q, Rvalue::Use(copy_local(w)));
    env.bump(&mut fb, b, q, 1);
    let release_disposing = |env: &Env, fb: &mut FuncBuilder, b: BlockId, w: Local| {
        let (h, k, one) = (fb.local(Ptr), fb.local(U64), fb.local(Bool));
        fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(w), int(-8, I64)));
        fb.assign(b, k, Rvalue::Use(copy_place(deref(h, U64))));
        fb.assign(b, one, bin(BinOp::Eq, copy_local(k), int(1, U64)));
        let (last, shared, next) = (fb.block(), fb.block(), fb.block());
        fb.branch(b, one, last, shared);
        let y = fb.local(I64);
        fb.assign(last, y, Rvalue::Use(copy_place(env.field(w, 1))));
        let args = vec![int(0, Ptr), copy_local(y)];
        let last = fb.call(last, Callee::Extern(env.keep), args, None);
        let args = vec![copy_local(h), int(24, U64), int(8, U64)];
        let after = fb.call(last, Callee::Extern(env.free), args, None);
        fb.goto(after, next);
        let dec = bin(BinOp::Sub, copy_local(k), int(1, U64));
        fb.push(shared, Stmt::Assign(deref(h, U64), dec));
        fb.goto(shared, next);
        next
    };
    let b = release_disposing(&env, &mut fb, b, q);
    let mark = |env: &Env, fb: &mut FuncBuilder, b: BlockId, k: i128| {
        let args = vec![int(0, Ptr), int(k, I64)];
        fb.call(b, Callee::Extern(env.keep), args, None)
    };
    let b = mark(&env, &mut fb, b, 101);
    let r = env.sum(&mut fb, b, w);
    let b = release_disposing(&env, &mut fb, b, w);
    let b = mark(&env, &mut fb, b, 102);
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    let p = check(env.pb.finish(), &[9], true);
    let (_, _, _, calls) = run_main(&p, &[9]);
    assert_eq!(calls, ["keep[101]", "keep[9]", "keep[102]"]);
}

#[test]
fn a_counted_field_of_a_replaced_object_is_released_where_the_object_dies() {
    // `inner = new(n, 3); o = new Holder(inner, 5); q = o (a retain); release q;
    // r = o.0.0 + o.1; release o` where o's last release releases `inner` (its drop glue) and
    // frees o. `o` goes to locals; `inner`, stored in o's field, stays on the heap and is still
    // released exactly once, when `o` dies.
    let mut env = env();
    let holder = env.pb.agg("Holder object", 16, 8, &[(Ptr, 0), (I64, 8)]);
    let hf = |w: Local, k: u32| Place {
        local: w,
        proj: vec![Proj::Deref(Ty::Agg(holder)), Proj::Field(k)],
    };
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let n = copy_local(fb.param(0));
    let (inner, o, base, q) = (fb.local(Ptr), fb.local(Ptr), fb.local(Ptr), fb.local(Ptr));
    let b = fb.block();
    let b = env.new_counted(&mut fb, b, inner, n, int(3, I64));
    let args = vec![int(24, U64), int(8, U64)];
    let b = fb.call(b, Callee::Extern(env.alloc), args, Some(base));
    fb.push(b, Stmt::Assign(deref(base, U64), Rvalue::Use(int(1, U64))));
    fb.assign(b, o, bin(BinOp::PtrAdd, copy_local(base), int(8, I64)));
    let fill = Stmt::MemSet {
        dst: copy_local(o),
        byte: int(0, U8),
        len: int(16, U64),
    };
    fb.push(b, fill);
    fb.push(b, Stmt::Assign(hf(o, 0), Rvalue::Use(copy_local(inner))));
    fb.push(b, Stmt::Assign(hf(o, 1), Rvalue::Use(int(5, I64))));
    fb.assign(b, q, Rvalue::Use(copy_local(o)));
    env.bump(&mut fb, b, q, 1);
    let release_holder = |env: &Env, fb: &mut FuncBuilder, b: BlockId, w: Local| {
        let (h, k, one, f) = (fb.local(Ptr), fb.local(U64), fb.local(Bool), fb.local(Ptr));
        fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(w), int(-8, I64)));
        fb.assign(b, k, Rvalue::Use(copy_place(deref(h, U64))));
        fb.assign(b, one, bin(BinOp::Eq, copy_local(k), int(1, U64)));
        let (last, shared, next) = (fb.block(), fb.block(), fb.block());
        fb.branch(b, one, last, shared);
        fb.assign(last, f, Rvalue::Use(copy_place(hf(w, 0))));
        let last = env.release(fb, last, f, 24);
        let args = vec![copy_local(h), int(24, U64), int(8, U64)];
        let after = fb.call(last, Callee::Extern(env.free), args, None);
        fb.goto(after, next);
        let dec = bin(BinOp::Sub, copy_local(k), int(1, U64));
        fb.push(shared, Stmt::Assign(deref(h, U64), dec));
        fb.goto(shared, next);
        next
    };
    let b = release_holder(&env, &mut fb, b, q);
    let (f, x, y, r) = (fb.local(Ptr), fb.local(I64), fb.local(I64), fb.local(I64));
    fb.assign(b, f, Rvalue::Use(copy_place(hf(o, 0))));
    fb.assign(b, x, Rvalue::Use(copy_place(env.field(f, 0))));
    fb.assign(b, y, Rvalue::Use(copy_place(hf(o, 1))));
    fb.assign(b, r, bin(BinOp::Add, copy_local(x), copy_local(y)));
    let b = release_holder(&env, &mut fb, b, o);
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    let original = env.pb.finish();
    let mut p = original.clone();
    let aggs = p.aggs.clone();
    let allocator = Allocator::find(&p);
    let last = p.funcs.len() - 1;
    assert!(run(&aggs, allocator, &mut p.funcs[last]).replaced, "{p}");
    assert_valid(&p);
    for n in [0, 4] {
        let (r0, allocs0, leak0, calls0) = run_main(&original, &[n]);
        let (r1, allocs1, leak1, calls1) = run_main(&p, &[n]);
        assert_eq!((r0, leak0, calls0), (r1, leak1, calls1), "n = {n}\n{p}");
        assert_eq!(
            (r1, allocs0, allocs1, leak1),
            (n + 5, 2, 1, 0),
            "n = {n}\n{p}"
        );
    }
}

/// A counted object whose class has a vtable (`Vt object`: the vtable in field 0, a number in
/// field 1) released through its vtable's drop slot, as lowering emits a release of an object
/// whose class has subclasses: `w = new(n); q = w (a retain); release q; between; r = w.1;
/// release w; return r`. The drop (`drop(w)`: `keep(0, w.1)`, a side effect, then free) is the
/// function at offset 64 of the vtable.
struct VtProgram {
    program: Program,
    /// The vtable `new` stores.
    vtable: StaticId,
}

fn vt_program(between: impl Fn(&mut FuncBuilder, BlockId, Local, AggId, StaticId)) -> VtProgram {
    let mut env = env();
    let vt = env.pb.agg("Vt object", 16, 8, &[(Ptr, 0), (I64, 8)]);
    let vf = |w: Local, k: u32| Place {
        local: w,
        proj: vec![Proj::Deref(Ty::Agg(vt)), Proj::Field(k)],
    };
    let mut db = FuncBuilder::internal("drop", &[Ptr], Unit);
    let (w, y, h) = (db.param(0), db.local(I64), db.local(Ptr));
    let b = db.block();
    db.assign(b, y, Rvalue::Use(copy_place(vf(w, 1))));
    let b = db.call(
        b,
        Callee::Extern(env.keep),
        vec![int(0, Ptr), copy_local(y)],
        None,
    );
    db.assign(b, h, bin(BinOp::PtrAdd, copy_local(w), int(-8, I64)));
    let args = vec![copy_local(h), int(24, U64), int(8, U64)];
    let b = db.call(b, Callee::Extern(env.free), args, None);
    db.ret(b, Operand::Const(Const::Unit, Unit));
    let drop = env.pb.add(db.finish());
    let vtable = env.pb.stat_with(&[0; 72], 8, vec![(64, Const::Func(drop))]);
    let other = env.pb.stat_with(&[0; 72], 8, vec![(64, Const::Func(drop))]);

    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let n = copy_local(fb.param(0));
    let (w, base, q, r) = (fb.local(Ptr), fb.local(Ptr), fb.local(Ptr), fb.local(I64));
    let b = fb.block();
    let args = vec![int(24, U64), int(8, U64)];
    let b = fb.call(b, Callee::Extern(env.alloc), args, Some(base));
    fb.push(b, Stmt::Assign(deref(base, U64), Rvalue::Use(int(1, U64))));
    fb.assign(b, w, bin(BinOp::PtrAdd, copy_local(base), int(8, I64)));
    let fill = Stmt::MemSet {
        dst: copy_local(w),
        byte: int(0, U8),
        len: int(16, U64),
    };
    fb.push(b, fill);
    let s = Operand::Const(Const::Static(vtable), Ptr);
    fb.push(b, Stmt::Assign(vf(w, 0), Rvalue::Use(s)));
    fb.push(b, Stmt::Assign(vf(w, 1), Rvalue::Use(n)));
    fb.assign(b, q, Rvalue::Use(copy_local(w)));
    let (h, c) = (fb.local(Ptr), fb.local(U64));
    fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(q), int(-8, I64)));
    fb.assign(b, c, Rvalue::Use(copy_place(deref(h, U64))));
    let inc = bin(BinOp::Add, copy_local(c), int(1, U64));
    fb.push(b, Stmt::Assign(deref(h, U64), inc));
    let release_vt = |fb: &mut FuncBuilder, b: BlockId, w: Local| {
        let (h, k, one) = (fb.local(Ptr), fb.local(U64), fb.local(Bool));
        let (t, slot, f) = (fb.local(Ptr), fb.local(Ptr), fb.local(Ptr));
        fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(w), int(-8, I64)));
        fb.assign(b, k, Rvalue::Use(copy_place(deref(h, U64))));
        fb.assign(b, one, bin(BinOp::Eq, copy_local(k), int(1, U64)));
        let (last, shared, next) = (fb.block(), fb.block(), fb.block());
        fb.branch(b, one, last, shared);
        fb.assign(last, t, Rvalue::Use(copy_place(vf(w, 0))));
        fb.assign(last, slot, bin(BinOp::PtrAdd, copy_local(t), int(64, I64)));
        fb.assign(last, f, Rvalue::Use(copy_place(deref(slot, Ptr))));
        let callee = Callee::Ptr {
            target: copy_local(f),
            params: vec![Ptr],
            ret: Unit,
        };
        let after = fb.call(last, callee, vec![copy_local(w)], None);
        fb.goto(after, next);
        let dec = bin(BinOp::Sub, copy_local(k), int(1, U64));
        fb.push(shared, Stmt::Assign(deref(h, U64), dec));
        fb.goto(shared, next);
        next
    };
    let b = release_vt(&mut fb, b, q);
    between(&mut fb, b, w, vt, other);
    fb.assign(b, r, Rvalue::Use(copy_place(vf(w, 1))));
    let b = release_vt(&mut fb, b, w);
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    VtProgram {
        program: env.pb.finish(),
        vtable,
    }
}

/// Run heap_sroa on `main`; returns what it did and the vtables `main` now names as operands.
fn vt_run(p: &mut Program) -> (Done, Vec<StaticId>) {
    let aggs = p.aggs.clone();
    let allocator = Allocator::find(p);
    let last = p.funcs.len() - 1;
    let done = run(&aggs, allocator, &mut p.funcs[last]);
    assert_valid(p);
    let mut named = vec![];
    for b in &p.funcs[last].blocks {
        let mut see = |op: &Operand| {
            if let Operand::Const(Const::Static(s), _) = op {
                named.push(*s);
            }
        };
        b.stmts
            .iter()
            .for_each(|s| crate::visit::stmt_operands(s, &mut see));
        crate::visit::term_operands(&b.term, &mut see);
    }
    (done, named)
}

#[test]
fn a_drop_through_a_known_vtable_reads_the_vtable_as_a_constant() {
    let VtProgram {
        program, vtable, ..
    } = vt_program(|_, _, _, _, _| {});
    let mut p = program.clone();
    let (done, named) = vt_run(&mut p);
    assert!(done.vtables && !done.replaced, "{p}");
    // The store in `new` and both releases' reads of the vtable.
    assert_eq!(named, [vtable, vtable, vtable], "{p}");
    for n in [0, 6] {
        assert_eq!(run_main(&program, &[n]), run_main(&p, &[n]), "{p}");
    }
}

#[test]
fn a_drop_through_a_known_vtable_ends_up_inlined_and_the_object_in_locals() {
    // The whole pipeline: the vtable read becomes the static, `vtable_loads` and `constfold`
    // make the call direct, the inliner inlines `drop` (its `keep`, the side effect, with it),
    // and heap_sroa then replaces the object.
    let VtProgram { program, .. } = vt_program(|_, _, _, _, _| {});
    let mut p = program.clone();
    crate::optimize(&mut p, crate::OptLevel::Speed);
    assert_valid(&p);
    for n in [0, 6] {
        let (r0, allocs0, leak0, calls0) = run_main(&program, &[n]);
        let (r1, allocs1, leak1, calls1) = run_main(&p, &[n]);
        assert_eq!((r0, leak0, &calls0), (r1, leak1, &calls1), "{p}");
        assert_eq!((allocs0, allocs1, leak1), (1, 0, 0), "{p}");
        assert_eq!(calls1, [format!("keep[{n}]")]);
    }
}

#[test]
fn an_object_whose_vtable_word_is_stored_again_keeps_its_vtable_loads() {
    // `w.0 = other` between the releases: the vtable is not known, so nothing is folded.
    let VtProgram {
        program, vtable, ..
    } = vt_program(|fb, b, w, vt, other| {
        let place = Place {
            local: w,
            proj: vec![Proj::Deref(Ty::Agg(vt)), Proj::Field(0)],
        };
        let s = Operand::Const(Const::Static(other), Ptr);
        fb.push(b, Stmt::Assign(place, Rvalue::Use(s)));
    });
    let mut p = program;
    let (done, named) = vt_run(&mut p);
    assert!(!done.vtables && !done.replaced, "{p}");
    assert!(named.iter().filter(|&&s| s == vtable).count() == 1, "{p}");
}

#[test]
fn an_object_read_before_its_vtable_is_stored_keeps_its_vtable_loads() {
    // A program that reads the vtable word before `new` stores it (read zero) must not see the
    // vtable there: the store-first check of the allocation's block fails.
    let VtProgram { mut program, .. } = vt_program(|_, _, _, _, _| {});
    let main = program.funcs.last_mut().expect("main");
    let vt = program.aggs.len() as u32 - 1;
    let (w, x) = (Local(1), Local(main.locals.len() as u32));
    let decl = main.locals[w.0 as usize].clone();
    main.locals.push(decl);
    let alloc_next = main
        .blocks
        .iter()
        .find_map(|b| match &b.term {
            Terminator::Call { next, .. } => Some(*next),
            _ => None,
        })
        .expect("the allocation");
    let read = Stmt::Assign(
        Place::local(x),
        Rvalue::Use(copy_place(Place {
            local: w,
            proj: vec![Proj::Deref(Ty::Agg(AggId(vt))), Proj::Field(0)],
        })),
    );
    // After the count, the pointer and the fill, before the vtable store.
    main.blocks[alloc_next.0 as usize].stmts.insert(3, read);
    let (done, _) = vt_run(&mut program);
    assert!(!done.vtables && !done.replaced, "{program}");
}
