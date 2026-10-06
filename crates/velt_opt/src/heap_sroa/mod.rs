//! Scalar replacement of heap objects that never escape their function (issue #533).
//!
//! `new Vec2(x, y)` allocates a class object with `velt_rt_alloc`, zero-fills it and writes
//! its fields; dropping the last owner calls `velt_rt_free`. When inlining has put an
//! object's whole life into one function (`p = p.add(v.scale(k))` with small methods), the
//! heap is not observable: nothing else ever sees the pointer. This pass then keeps the object
//! in an aggregate local (which `sroa` splits into registers right after) and removes the
//! allocation, the zero fill and the free, as escape analysis does in JIT compilers, but at
//! compile time.
//!
//! **Webs.** Pointer locals connected by copies (`a = b`) form a *web*; the objects a web
//! holds are the ones its allocations created. A web is replaced only when every mention of
//! its locals is one of:
//! - `w = velt_rt_alloc(size, align)`, `w = null` or `w = v` (`v` in the same web);
//! - a place through the pointer, `(*w as Obj)…`, read or written (always as the same object
//!   aggregate `Obj`, with constant sizes matching it);
//! - `memset w, 0, size(Obj)` (the zero fill of `new`), `velt_rt_free(w, size, align)`;
//! - `w == null` / `w != null` (drop guards; the pointer stays as a non-null token).
//!
//! So the pointer is never stored in memory, passed to a call, returned, offset, compared
//! with another pointer (identity is not observable) or turned into the address of a field:
//! the object does not escape, and its reference count, if it had one, would be observable
//! only through those (counted objects are reached through an offset pointer anyway).
//!
//! **Value semantics.** After the rewrite each local of a web carries its own copy of the
//! object (`w_obj`), and `a = b` copies it. That equals the reference semantics as long as a
//! write through one local is never observed through another local holding the same object.
//! `flow` checks exactly that: at every write through `w`, every other local that may hold
//! `w`'s object (a forward may-alias analysis over the copies) is dead for reads through it
//! (a backward liveness in which a copy `a = b` keeps `b` alive while `a` is). A borrowed
//! `this = p` of an inlined method that only reads, `p = new …` in a loop whose old value is
//! read before it is freed, and constructors writing a fresh object all pass; a method that
//! writes `this` while the caller still reads `p` afterwards does not (yet).
//!
//! Statement order, and with it the order of every call and drop, is unchanged; only the
//! allocator calls disappear, in pairs (an object of a replaced web is freed, if at all, only
//! through the web). Debug builds do not run `velt_opt`, so the checking allocator still sees
//! every object there.

mod flow;
mod rewrite;
mod webs;

use velt_vir::vir::{AggLayout, ExternId, Function, Program, Ty};

/// The runtime allocator's externs, as lowering declares them (rt_abi.md).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Allocator {
    /// `velt_rt_alloc(size: u64, align: u64) -> ptr`.
    pub alloc: ExternId,
    /// `velt_rt_free(p: ptr, size: u64, align: u64)`.
    pub free: ExternId,
}

impl Allocator {
    /// The allocator externs of `program`, if it declares both.
    pub fn find(program: &Program) -> Option<Allocator> {
        let find = |symbol: &str, params: &[Ty], ret: Ty| {
            program
                .externs
                .iter()
                .position(|e| e.symbol == symbol && e.params == params && e.ret == ret)
                .map(|i| ExternId(i as u32))
        };
        Some(Allocator {
            alloc: find("velt_rt_alloc", &[Ty::U64, Ty::U64], Ty::Ptr)?,
            free: find("velt_rt_free", &[Ty::Ptr, Ty::U64, Ty::U64], Ty::Unit)?,
        })
    }
}

/// Replace the non-escaping heap objects of `func`; returns whether anything changed.
pub(crate) fn run(aggs: &[AggLayout], allocator: Option<Allocator>, func: &mut Function) -> bool {
    let Some(allocator) = allocator else {
        return false;
    };
    let Some(mut webs) = webs::scan(aggs, allocator, func) else {
        return false;
    };
    flow::check(allocator, func, &mut webs);
    if !webs.any() {
        return false;
    }
    rewrite::apply(aggs, allocator, func, &webs);
    true
}

#[cfg(test)]
mod random_tests;
#[cfg(test)]
mod tests;
