//! Array views (#1485; std/prelude/zzz_adapt.vlt): an `any` holding an array of another
//! element type, converted to `U[]` (`kids as JSXChild[]`, also as a union member, and a cast
//! `xs as U[]` of an existing array, which sema writes through `any`), is a view of that one
//! array, as in Node, not a copy.
//!
//! Only the *view types* can hold a view: the array types an `any` is converted to in the
//! program's own code, and the arrays among their element types (nested arrays, a recursive
//! union such as `JSXChild`). They are boxed. A view is a box whose header is
//! `{ data: view | 1, len: 0, cap: 0 }`, `view` the prelude's `__ArrayView<U>`: the original
//! array as an `any` (`__target`) and a mirror `U[]` of its elements (`__seen`). No pointer is
//! tagged, so retain and release are those of any box. On a view type:
//!
//! | operation | `data & 1` clear | `data & 1` set (a view) |
//! |---|---|---|
//! | `length` | the header's | the original's (`__arrayViewLen`) |
//! | a read or write of element `i` (an index, a `for...of` step) | the header | the mirror, element `i` synced with the original first (`__arrayViewSyncAt`); a write goes back for element `i` only (`__arrayViewFlushAt`) |
//! | `push`, `pop`, `truncate`, swaps | the change | the change on the mirror, synced for its length (and the elements swapped or popped), then the changed elements written back |
//! | any other access to the contents (`content`) | the header | the mirror, synced with the original first (`__arrayViewSync`) |
//! | any other change | the change | the change on the mirror, then written back (`__arrayViewFlush`) |
//! | the stable sort (`sort(cmp)`) | the sort | a sorted copy of the mirror as the mirror, written back (`__arrayViewSort`) |
//! | to `any` | the array | the original (`__target`) |
//! | drop | the elements and buffer | the view object, and the view's entry in the table |
//! | deep copy, transfer | the elements | a plain array of the mirror's elements (no longer in the table) |
//!
//! So a loop over a view costs a plain loop plus a conversion of each element it reaches,
//! and a view converts only the elements a read reaches (and those before them).
//!
//! **Identity.** An array has at most one live view of each view type: the runtime keeps a
//! table from (the original's data pointer, the view type) to the view's box (rt_abi.md
//! "Views"), which a conversion asks first, as #1491's object views do. So two conversions of
//! one array to `U[]` are one value (`===`, `indexOf`, `Set` and `Map` keys), sharing one
//! mirror. The view's drop removes its entry; a view that stops being one (a transfer to
//! another thread makes it a plain array) leaves the table first.
//!
//! A program without such a conversion has no view types and compiles as before.

use rustc_hash::FxHashSet;
use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::any_set_site::ARRAY_ID;
use super::boxing::Boxing;
use super::operand::proj;
use super::rt::Rt;
use super::{cint, ice, Cx, FnLower};
use crate::vir::{self, BinOp, Callee, Const, Operand, Place, Proj, Rvalue, Ty};

pub(super) const VIEW_CLASS: &str = "std/prelude/zzz_adapt::__ArrayView";
const VIEW_NEW: &str = "std/prelude/zzz_adapt::__arrayViewNew";
const VIEW_SYNC: &str = "std/prelude/zzz_adapt::__arrayViewSync";
const VIEW_SYNC_AT: &str = "std/prelude/zzz_adapt::__arrayViewSyncAt";
const VIEW_SYNC_LEN: &str = "std/prelude/zzz_adapt::__arrayViewSyncLen";
const VIEW_FLUSH: &str = "std/prelude/zzz_adapt::__arrayViewFlush";
const VIEW_FLUSH_AT: &str = "std/prelude/zzz_adapt::__arrayViewFlushAt";
const VIEW_FLUSH_FROM: &str = "std/prelude/zzz_adapt::__arrayViewFlushFrom";
pub(super) const VIEW_LEN: &str = "std/prelude/zzz_adapt::__arrayViewLen";
pub(super) const VIEW_SORT: &str = "std/prelude/zzz_adapt::__arrayViewSort";

/// What an access to a view's contents needs of the mirror (`content_with`).
pub(super) enum ViewSync {
    /// Every element (`__arrayViewSync`).
    All,
    /// The original's length, the new elements converted (`__arrayViewSyncLen`).
    Len,
    /// That, and the last element up to date (a `pop`).
    Last,
    /// The elements at these indices (`u64` operands) up to date (`__arrayViewSyncAt`).
    Slots(Vec<Operand>),
}

/// What a change through a view writes back to the original (`view_flush`).
pub(super) enum ViewFlush {
    /// Every element, and the length (`__arrayViewFlush`).
    All,
    /// The elements from this index (`u64`) on, and the length (`__arrayViewFlushFrom`).
    From(Operand),
    /// The elements at these indices (`u64` operands) (`__arrayViewFlushAt`).
    Slots(Vec<Operand>),
}

/// The number the runtime's table of array views keys the view type `t` by.
fn view_key(t: TyId) -> Operand {
    cint(t.0 as i128, Ty::U64)
}

impl Cx<'_> {
    /// May a value of the array type `t` be a view?
    pub(super) fn array_view(&self, t: TyId) -> bool {
        !self.boxing.array_views.is_empty() && self.boxing.array_views.contains(&t)
    }

    /// May an array of elements `e` be a view?
    pub(super) fn array_view_of(&mut self, e: TyId) -> bool {
        if self.boxing.array_views.is_empty() {
            return false;
        }
        let t = self.intern(TyKind::Array(e));
        self.array_view(t)
    }

    /// Record an `AnyTo` to the array type `t` (not counted in the standard library).
    pub(super) fn note_array_view(&mut self, t: TyId) {
        let std = self.cur_def.is_some_and(|d| match self.hir.def(d) {
            hir::Def::Fn(f) => f.name.starts_with("std/"),
            _ => false,
        });
        let TyKind::Array(_) = self.types.kind(t) else {
            return;
        };
        if std {
            return;
        }
        self.facts.array_view_targets.insert(t);
    }

    /// The view types implied by this pass's facts: the targets and the arrays their elements
    /// hold, each boxed.
    pub(super) fn close_array_views(&mut self, next: &mut Boxing) {
        let mut work: Vec<TyId> = self.facts.array_view_targets.iter().copied().collect();
        if work.is_empty() || self.view_def(VIEW_NEW).is_none() {
            return;
        }
        let mut seen = FxHashSet::default();
        while let Some(t) = work.pop() {
            let TyKind::Array(e) = self.types.kind(t).clone() else {
                continue;
            };
            if next.array_views.insert(t) {
                next.boxes.insert(t);
                self.inner_arrays(e, &mut work, &mut seen);
            }
        }
    }

    /// The array types a value of `t` holds as itself, an option's payload or a union member.
    fn inner_arrays(&mut self, t: TyId, out: &mut Vec<TyId>, seen: &mut FxHashSet<TyId>) {
        if !seen.insert(t) {
            return;
        }
        match self.types.kind(t).clone() {
            TyKind::Array(_) => out.push(t),
            TyKind::Option(x, _) => self.inner_arrays(x, out, seen),
            TyKind::Adt(d, _) if self.is_union(t) => {
                let n = self.enum_def(d).variants.len() as u32;
                for v in 0..n {
                    for p in self.variant_tys(t, v) {
                        self.inner_arrays(p, out, seen);
                    }
                }
            }
            _ => {}
        }
    }

    /// The prelude definition `name` (std/prelude/zzz_adapt.vlt).
    fn view_def(&self, name: &str) -> Option<DefId> {
        let found = self.hir.defs.iter().position(|d| match d {
            hir::Def::Fn(f) => f.name == name,
            hir::Def::Adt(a) => a.name == name,
            _ => false,
        });
        found.map(|i| DefId(i as u32))
    }
}

impl FnLower<'_, '_> {
    fn view_call(&mut self, name: &str, e: TyId, args: Vec<Operand>, out: Option<Place>) {
        self.view_call_with(name, vec![e], args, out);
    }

    /// A call of the prelude function `name` instantiated with `targs`.
    pub(super) fn view_call_with(
        &mut self,
        name: &str,
        targs: Vec<TyId>,
        args: Vec<Operand>,
        out: Option<Place>,
    ) {
        let d = self
            .cx
            .view_def(name)
            .unwrap_or_else(|| ice(format_args!("no prelude `{name}`")));
        let f = self.cx.func_for_direct(d, targs, self.mode);
        self.call(Callee::Func(f), args, out, false);
    }

    /// The type `__ArrayView<e>`.
    fn view_class(&mut self, e: TyId) -> TyId {
        let d = self
            .cx
            .view_def(VIEW_CLASS)
            .unwrap_or_else(|| ice("no prelude `__ArrayView`"));
        self.cx.intern(TyKind::Adt(d, vec![e]))
    }

    /// The place of field `name` of the `__ArrayView<e>` object `v` (untagged).
    fn view_field(&mut self, v: &Operand, e: TyId, name: &str) -> Place {
        let cls = self.view_class(e);
        let TyKind::Adt(d, _) = self.cx.kind(cls) else {
            ice("`__ArrayView` is not a class")
        };
        let fields = &self.cx.adt_def(d).fields;
        let Some(i) = fields.iter().position(|f| f.name == name) else {
            ice(format_args!("`__ArrayView` without `{name}`"))
        };
        self.proxy_hot_place(v, cls, i as u32)
    }

    /// The view object of the header at `hdr` when it is a view: `(is_view, untagged)`.
    pub(super) fn array_view_of_hdr(&mut self, hdr: &Place) -> (Operand, Operand) {
        let d = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(hdr, Proj::Field(0)))),
        );
        let is = self.proxy_tagged(d.clone());
        (is, self.proxy_untag(d))
    }

    /// In `AnyTo` to the view type `ty`, of the `any` at `p` (owned) holding no `ty`: when it
    /// holds an array (or a tuple), a view of it, moved into `out`, and on to `done`;
    /// anything else goes on in the current block.
    pub(super) fn array_view_make(
        &mut self,
        p: &Place,
        desc: Operand,
        ty: TyId,
        out: &Place,
        done: vir::BlockId,
    ) {
        let id = self.vtable_class_id(desc);
        let want = cint(ARRAY_ID as i128, Ty::U64);
        let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, id, want));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is, yes, no);
        self.switch_to(yes);
        let v = self.array_view_new(p, ty);
        self.assign(out.clone(), Rvalue::Use(v));
        self.goto(done);
        self.switch_to(no);
    }

    /// The view of type `ty` of the array the `any` at `p` holds (moved in): the array's live
    /// view of that type (a new reference to it; the `any` is released), or a new one, which
    /// becomes its live view.
    pub(super) fn array_view_new(&mut self, p: &Place, ty: TyId) -> Operand {
        let e = self.elem_ty(ty);
        let data = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(proj(p, Proj::Field(0)))));
        let out = Place::local(self.temp(Ty::Ptr));
        let live = Place::local(self.temp(Ty::Ptr));
        let key = view_key(ty);
        self.call_rt(
            Rt::ArrayViewGet,
            vec![data.clone(), key.clone()],
            Some(live.clone()),
        );
        let has = self.non_null(Operand::Copy(live.clone()));
        let (hit, miss, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(has, hit, miss);
        self.switch_to(hit);
        let kept = self.share_value(Operand::Copy(live), ty);
        let any = self.cx.any_ty();
        self.drop_glue(p.clone(), any);
        self.assign(out.clone(), Rvalue::Use(kept));
        self.goto(join);
        self.switch_to(miss);
        let arg = self.addr(p.clone());
        let r = self.temp(Ty::Ptr);
        self.view_call(VIEW_NEW, e, vec![arg], Some(Place::local(r)));
        let tagged = self.proxy_tag(Operand::Copy(Place::local(r)));
        let a = self.cx.array_agg();
        let parts = vec![tagged, cint(0, Ty::U64), cint(0, Ty::U64)];
        let hdr = self.rvalue_temp(Ty::Agg(a), Rvalue::Aggregate(a, parts));
        let bx = self.box_value(hdr, ty);
        let bx = self.rvalue_temp(Ty::Ptr, Rvalue::Use(bx));
        self.call_rt(Rt::ArrayViewPut, vec![data, key, bx.clone()], None);
        self.assign(out.clone(), Rvalue::Use(bx));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(out)
    }

    /// The view at `arr` (the inline value, in its box) of an array of `e`, whose view object
    /// is `v`, leaves the runtime's table (it is dropped, or stops being a view).
    fn view_unlist(&mut self, arr: &Place, e: TyId, v: &Operand) {
        let t = self.cx.intern(TyKind::Array(e));
        let target = self.view_field(v, e, "__target");
        let data = Operand::Copy(proj(&target, Proj::Field(0)));
        let me = self.addr(arr.clone());
        self.call_rt(Rt::ArrayViewRemove, vec![data, view_key(t), me], None);
    }

    /// The header to operate on for the inline value `hdr` of the view type `t`: the header
    /// itself, or for a view its mirror, brought up to date with the original as `sync` needs.
    pub(super) fn view_inline(&mut self, hdr: Place, t: TyId, sync: ViewSync) -> Place {
        let e = self.elem_ty(t);
        let (is, v) = self.array_view_of_hdr(&hdr);
        let at = Place::local(self.temp(Ty::Ptr));
        let here = self.addr(hdr);
        self.assign(at.clone(), Rvalue::Use(here));
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(is, yes, join);
        self.switch_to(yes);
        let flag = |b| Operand::Const(Const::Bool(b), Ty::Bool);
        match sync {
            ViewSync::All => self.view_call(VIEW_SYNC, e, vec![v.clone()], None),
            ViewSync::Len => self.view_call(VIEW_SYNC_LEN, e, vec![v.clone(), flag(false)], None),
            ViewSync::Last => self.view_call(VIEW_SYNC_LEN, e, vec![v.clone(), flag(true)], None),
            ViewSync::Slots(at) => {
                for i in at {
                    self.view_call(VIEW_SYNC_AT, e, vec![v.clone(), i], None);
                }
            }
        }
        let seen = self.view_field(&v, e, "__seen");
        self.assign(at.clone(), Rvalue::Use(Operand::Copy(seen)));
        self.goto(join);
        self.switch_to(join);
        let payload = self.cx.payload_ty(t);
        proj(&at, Proj::Deref(payload))
    }

    /// After a change through the array of the view type `t` whose box pointer is `bx`: a
    /// view writes its mirror back to the original, as much of it as `what` says changed.
    pub(super) fn view_flush(&mut self, bx: Operand, t: TyId, what: ViewFlush) {
        if self.dead() {
            return;
        }
        let e = self.elem_ty(t);
        let payload = self.cx.payload_ty(t);
        let pp = self.operand_place(bx, Ty::Ptr);
        let (is, v) = self.array_view_of_hdr(&proj(&pp, Proj::Deref(payload)));
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(is, yes, join);
        self.switch_to(yes);
        match what {
            ViewFlush::All => self.view_call(VIEW_FLUSH, e, vec![v], None),
            ViewFlush::From(i) => self.view_call(VIEW_FLUSH_FROM, e, vec![v, i], None),
            ViewFlush::Slots(at) => {
                for i in at {
                    self.view_call(VIEW_FLUSH_AT, e, vec![v.clone(), i], None);
                }
            }
        }
        self.goto(join);
        self.switch_to(join);
    }

    /// About to write into the counted object of type `ty` whose pointer `p` holds, as part of
    /// an assignment target (rc.rs `write_barrier`, the lock set; a view's write-back).
    pub(super) fn target_barrier(&mut self, p: &Place, ty: TyId) {
        if self.cx.array_view(ty) && !self.read_target && !self.dead() {
            let bx = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(p.clone())));
            if let Some(w) = &mut self.view_writes {
                w.push((bx, ty, None));
            }
        }
        if !self.read_target && self.cx.lock_counted(ty) && !self.dead() {
            self.write_barrier(Operand::Copy(p.clone()), ty);
        }
    }

    /// `f` (an assignment), with the views its target goes through written back after it.
    pub(super) fn with_view_writes(&mut self, f: impl FnOnce(&mut Self) -> Operand) -> Operand {
        if self.cx.boxing.array_views.is_empty() {
            return f(self);
        }
        let outer = self.view_writes.replace(vec![]);
        let r = f(self);
        let ws = std::mem::replace(&mut self.view_writes, outer).unwrap_or_default();
        for (bx, t, at) in ws {
            let what = match at {
                Some(i) => ViewFlush::Slots(vec![i]),
                None => ViewFlush::All,
            };
            self.view_flush(bx, t, what);
        }
        r
    }

    /// The index operand `i` (of VIR type `ity`) as the `u64` the view helpers take.
    pub(super) fn view_index(&mut self, i: Operand, ity: Ty) -> Operand {
        let iu = self.cast_to(i, ity, Ty::U64);
        self.rvalue_temp(Ty::U64, Rvalue::Use(iu))
    }

    /// The target of the assignment being lowered is element `iu` of the view type `t`'s
    /// array, whose box pointer the last entry of `view_writes` since `mark` holds (when the
    /// barrier made one): only that element is written back.
    pub(super) fn view_write_at(&mut self, mark: usize, t: TyId, iu: Operand) {
        if let Some(w) = &mut self.view_writes {
            if w.len() == mark + 1 && w[mark].1 == t {
                w[mark].2 = Some(iu);
            }
        }
    }

    /// The contents of the array at `arr` of type `t` for a read (or, with `mark`, an
    /// assignment target) of element `index`, whose value `given` may hold already: a view
    /// brings only that element up to date, the index evaluated first unless that may run code
    /// that writes (then every element). Answers the index's value when it is evaluated here.
    pub(super) fn index_content(
        &mut self,
        arr: &Place,
        t: TyId,
        index: &hir::Expr,
        given: Option<Operand>,
        mark: Option<usize>,
    ) -> (Place, Option<Operand>) {
        if !self.cx.array_view(t) || (given.is_none() && self.index_may_write(index)) {
            return (self.content(arr, t), given);
        }
        let i = match given {
            Some(i) => i,
            None => self.expr(index),
        };
        let ity = self.vty(index.ty);
        let iu = self.view_index(i.clone(), ity);
        if let Some(mark) = mark {
            self.view_write_at(mark, t, iu.clone());
        }
        let c = self.content_with(arr, t, ViewSync::Slots(vec![iu]));
        (c, Some(i))
    }

    /// [`Self::may_write`] of an index, where a `number` index's conversion
    /// (`__floatIndex(i)`, sema `float_index`) changes nothing.
    fn index_may_write(&mut self, index: &hir::Expr) -> bool {
        if let hir::ExprKind::Call {
            callee: hir::Callee::Def(d, _),
            args,
        } = &index.kind
        {
            let float_index = matches!(self.cx.hir.def(*d), hir::Def::Fn(f) if f.name.ends_with("::__floatIndex"));
            if let (true, [i]) = (float_index, &args[..]) {
                return self.may_write(i);
            }
        }
        self.may_write(index)
    }

    /// The number of entries in `view_writes` (for [`Self::view_write_at`]).
    pub(super) fn view_writes_mark(&self) -> usize {
        self.view_writes.as_ref().map_or(0, Vec::len)
    }

    /// The `(data, table)` of the `any` for the box `data` (owned) of the view type `t`, whose
    /// own table is `table`: for a view, the original array (a new reference; the view is
    /// released).
    pub(super) fn view_any_parts(
        &mut self,
        data: Operand,
        table: Operand,
        t: TyId,
    ) -> (Operand, Operand) {
        let e = self.elem_ty(t);
        let payload = self.cx.payload_ty(t);
        let bx = Place::local(self.copy_to_temp(data, Ty::Ptr));
        let (d, tb) = (self.temp(Ty::Ptr), self.temp(Ty::Ptr));
        self.assign(Place::local(d), Rvalue::Use(Operand::Copy(bx.clone())));
        self.assign(Place::local(tb), Rvalue::Use(table));
        let pp = self.operand_place(Operand::Copy(bx.clone()), Ty::Ptr);
        let (is, v) = self.array_view_of_hdr(&proj(&pp, Proj::Deref(payload)));
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(is, yes, join);
        self.switch_to(yes);
        let target = self.view_field(&v, e, "__target");
        let any = self.cx.any_ty();
        let shared = self.share_value(Operand::Copy(target), any);
        let avt = self.cx.ty(any);
        let held = Place::local(self.copy_to_temp(shared, avt));
        self.assign(
            Place::local(d),
            Rvalue::Use(Operand::Copy(proj(&held, Proj::Field(0)))),
        );
        self.assign(
            Place::local(tb),
            Rvalue::Use(Operand::Copy(proj(&held, Proj::Field(1)))),
        );
        self.drop_glue(bx, t);
        self.goto(join);
        self.switch_to(join);
        (
            Operand::Copy(Place::local(d)),
            Operand::Copy(Place::local(tb)),
        )
    }

    /// In the drop of the inline value `arr` of an array of `e`: when it is a view, the view
    /// object is released, and `true` is answered (the elements and buffer are the mirror's).
    /// The current block then continues after the drop on both paths.
    pub(super) fn view_drop(&mut self, arr: &Place, e: TyId, rest: impl FnOnce(&mut Self)) {
        if !self.cx.array_view_of(e) {
            return rest(self);
        }
        let (is, v) = self.array_view_of_hdr(arr);
        let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is, yes, no);
        self.switch_to(yes);
        self.view_unlist(arr, e, &v);
        let cls = self.view_class(e);
        let vp = Place::local(self.copy_to_temp(v, Ty::Ptr));
        self.drop_glue(vp, cls);
        self.goto(join);
        self.switch_to(no);
        rest(self);
        self.goto(join);
        self.switch_to(join);
    }

    /// Before a deep copy or transfer of the inline value at `arr` of an array of `e`: a view
    /// becomes a plain array of its mirror's elements (deep-copied), and the view is released.
    pub(super) fn view_materialize(&mut self, arr: &Place, e: TyId) {
        if !self.cx.array_view_of(e) {
            return;
        }
        let t = self.cx.intern(TyKind::Array(e));
        let (is, v) = self.array_view_of_hdr(arr);
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(is, yes, join);
        self.switch_to(yes);
        let m = self.view_inline(arr.clone(), t, ViewSync::All);
        let a = self.cx.array_agg();
        let fresh = Place::local(self.temp(Ty::Agg(a)));
        self.clone_array(&m, &fresh, e);
        self.view_unlist(arr, e, &v);
        let cls = self.view_class(e);
        let vp = Place::local(self.copy_to_temp(v, Ty::Ptr));
        self.drop_glue(vp, cls);
        self.assign(arr.clone(), Rvalue::Use(Operand::Copy(fresh)));
        self.goto(join);
        self.switch_to(join);
    }
}

impl FnLower<'_, '_> {
    /// Before an operation that moves elements' bits between arrays (the stable sort's
    /// `ArrayMove` / `ArraySetLen`) on the array of type `t` whose box pointer `bx` holds: a view
    /// stops with a `TypeError`, since its mirror must stay whole between write-backs.
    pub(super) fn view_unsupported(&mut self, bx: &Place, t: TyId, what: &str) {
        if !self.cx.array_view(t) || self.dead() {
            return;
        }
        let payload = self.cx.payload_ty(t);
        let pp = self.operand_place(Operand::Copy(bx.clone()), Ty::Ptr);
        let (is, _) = self.array_view_of_hdr(&proj(&pp, Proj::Deref(payload)));
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(is, yes, join);
        self.switch_to(yes);
        self.panic_msg(&format!(
            "TypeError: {what} on an array view (an array converted to another element type) is not supported yet (#1485)"
        ));
        self.terminate(crate::vir::Terminator::Unreachable);
        self.switch_to(join);
    }
}

impl FnLower<'_, '_> {
    /// In `AnyTo` to the union `ty` (`n` variants) of the `any` at `p` (owned) that holds none
    /// of its members: when the union has an array member that is a view type and the `any`
    /// holds an array, a view of it as that member, into `out`, and on to `done`.
    pub(super) fn union_array_view(
        &mut self,
        p: &Place,
        desc: Operand,
        ty: TyId,
        n: u32,
        out: &Place,
        done: vir::BlockId,
    ) {
        for v in 0..n {
            let [pt] = self.cx.variant_tys(ty, v)[..] else {
                continue;
            };
            if matches!(self.cx.kind(pt), TyKind::Array(_)) {
                self.cx.note_array_view(pt);
            }
            if !self.cx.array_view(pt) {
                continue;
            }
            let id = self.vtable_class_id(desc);
            let want = cint(ARRAY_ID as i128, Ty::U64);
            let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, id, want));
            let (yes, no) = (self.new_block(), self.new_block());
            self.branch(is, yes, no);
            self.switch_to(yes);
            let val = self.array_view_new(p, pt);
            let view = self.cx.view(ty, v);
            let vp = proj(out, Proj::Cast(view));
            self.assign(
                proj(&vp, Proj::Field(0)),
                Rvalue::Use(cint(v as i128, Ty::U32)),
            );
            self.assign(proj(&vp, Proj::Field(1)), Rvalue::Use(val));
            self.goto(done);
            self.switch_to(no);
            return;
        }
    }
}
