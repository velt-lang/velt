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
//! **Webs.** Pointer locals connected by copies (`a = b`) and constant offsets (`a = b + 8`) form
//! a *web*; the objects a web holds are the ones its allocations created. A web is replaced only
//! when every mention of its locals is one of:
//! - `w = velt_rt_alloc(size, align)`, `w = null`, `w = v` or `w = v + c` (`v` in the same web);
//! - a place through the pointer, `(*w as Obj)…`, read or written (as the object aggregate `Obj`
//!   or one laid out as it, `Uint8Array` read as its base `BufferView`, at one offset in the
//!   block, with constant sizes matching it);
//! - for a counted object (the block is a `u64` count, then the object, #1718): `(*h as u64)`
//!   with `h` at the start of the block, read or written whole (a retain, a release, the count's
//!   flag bits tested), and `velt_rt_free(h, 8 + size(Obj), align)`;
//! - `memset w, 0, size(Obj)` (the zero fill of `new`), `velt_rt_free(w, size, align)`;
//! - `w == null` / `w != null` (drop guards; the pointer stays as a non-null token).
//!
//! So the pointer is never stored in memory, passed to a call, returned, compared with another
//! pointer (identity is not observable) or turned into the address of a field: the object does
//! not escape. Its count is then data like its fields, read and written only by this function's
//! retains and releases: it moves into a local (`w.rc`) next to the object's (`w.obj`), and the
//! tests of its last reference fold away once it is constant. What holds an object by its address
//! (a weak table, a `WeakRef`, the cycle collector's buffer, another thread) is given it in a call,
//! so such an object stays on the heap. A field holding a counted value is no exception: lowering
//! drops and shares a pointer field through the address of a copy (`old = (*w).f; drop(&old)`), a
//! read of the field, so the object's own counted fields (a view's buffer) are released where
//! the object would have died.
//!
//! **Known vtables** (`devirt`). A counted object's drop and its virtual getters run as calls
//! through its vtable that are given the object; when every object of an otherwise replaceable
//! web got the same vtable, its loads become that vtable, the calls become direct (`vtable_loads`,
//! `constfold`, in the same round), the next round inlines them and the one after replaces the
//! object. **Cost** (`cost`): a counted web whose copies sit in deeper loops than all of its
//! allocations (an error object a hot loop carries a pointer for) stays on the heap, where a copy
//! is a pointer's and not the object's.
//!
//! **Value semantics.** After the rewrite each local of a web carries its own copy of the
//! object (`w_obj`), and `a = b` copies it. That equals the reference semantics as long as
//! every write through one local is seen by the other locals that still read the same object.
//! `flow` checks that, and says where to copy: at every write through `w`, each other local
//! that may hold `w`'s object (a forward may-alias analysis over the copies) and is still read
//! through (a backward liveness in which a copy `a = b` keeps `b` alive while `a` is) must hold
//! it on every path (a forward must-alias analysis); the rewrite then copies `w_obj` to it
//! right after the write. One object under two names (the constructor's temporary and the
//! variable, a variable and an inlined method's `this`) is a must-alias, so a method writing
//! `this` while the caller reads the variable afterwards is replaced. A local that holds the
//! object only on some paths (`q = cond ? a : b`, then a write through `a` and a read of `q`)
//! keeps the web on the heap: no copy is right on every path.
//!
//! Statement order, and with it the order of every call and drop, is unchanged; only the
//! allocator calls disappear, in pairs (an object of a replaced web is freed, if at all, only
//! through the web). Debug builds do not run `velt_opt`, so the checking allocator still sees
//! every object there.

mod aliases;
mod cost;
mod devirt;
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

/// What [`run`] did to a function.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Done {
    /// Objects were replaced.
    pub replaced: bool,
    /// Vtables of objects kept on the heap became known (`devirt`): `vtable_loads` and
    /// `constfold` can make their calls direct now, for the next round to inline.
    pub vtables: bool,
}

impl Done {
    pub fn any(self) -> bool {
        self.replaced || self.vtables
    }
}

/// Replace the non-escaping heap objects of `func`.
pub(crate) fn run(aggs: &[AggLayout], allocator: Option<Allocator>, func: &mut Function) -> Done {
    let Some(allocator) = allocator else {
        return Done::default();
    };
    let Some(mut webs) = webs::scan(aggs, allocator, func) else {
        return Done::default();
    };
    let vtables = devirt::run(allocator, func, &webs);
    let mut updates = flow::check(allocator, func, &mut webs);
    cost::check(allocator, func, &mut webs, &updates);
    updates.retain(|u| webs.obj(u.dst).is_some() && webs.obj(u.src).is_some());
    let replaced = webs.any();
    if replaced {
        rewrite::apply(aggs, allocator, func, &webs, &updates);
    }
    Done { replaced, vtables }
}

#[cfg(test)]
mod random_tests;
#[cfg(test)]
mod tests;
