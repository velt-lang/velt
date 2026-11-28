//! Record views (#379; std/lang/hooks/any_record.velt, std/prelude/record.velt): `x as
//! Record<string, any>` of an `any` holding an object or another record is a view of that one
//! object, as in Node, not the copy `anyToRecordAny` makes (any_to_record.rs).
//!
//! Only the *view types* can hold a view: the record types with `any` values an `any` is
//! converted to in the program's own code (`unknown` values are `any` ones by then). A view is a
//! record whose entries (field 0) are a mirror of the source's, and whose field 4 (`view`, 0 in
//! any other record) holds a counted pointer to the hook object `RecordViewSource`, which holds
//! the source as an `any`. std's `Record` methods call the `recordView*` intrinsics: in a view
//! type, `recordViewIs` tests field 4, and for a view a keyed read or write goes to the source
//! through `any` (`recordViewGetOne` and the other hooks), while a read of the whole record first
//! rebuilds the mirror (`recordViewSyncAll`). In any other record type `recordViewIs` is `false`
//! and the others are no code, so a program without views compiles as before.
//!
//! | operation on a view | done as |
//! |---|---|
//! | `r[k]`, `Object.hasOwn(r, k)` | the source's own entry `k`, through `any` |
//! | `k in r` | `k in` the source, through `any`: what it inherits too (a class's methods) |
//! | `Object.keys` / `values` / `entries`, `size`, the glue that reads field 0 | the mirror rebuilt from the source, in its order, then the mirror's |
//! | `r[k] = v`, `delete r[k]` | written to the source, or deleted from it, through `any` |
//! | a spread or `Object.assign` into it | the mirror's, then the spread's keys written to the source |
//! | deep copy, transfer | a plain record of the mirror (field 4 none) |
//!
//! **Identity.** An object has at most one live view: the runtime's object view table
//! (rt_abi.md "Views") keys it by the object's address with bit 0 set, which no typed proxy's
//! entry (keyed by the address itself) can be. So two conversions of one object are one value
//! (`===`). A view's drop removes its entry before it releases the source.

use velt_sema::hir::{self, DefId, Intrinsic, TyId, TyKind};

use super::boxing::Boxing;
use super::hook_calls::{self as hooks, ty, HookTy};
use super::operand::proj;
use super::rt::Rt;
use super::{cint, unit, Cx, FnLower};
use crate::vir::{BinOp, BlockId, Const, Operand, Place, Proj, Rvalue, Ty};

/// Whether the program has record views at all (its calls: `hook_calls.rs`).
const VIEW_NEW: &str = velt_common::std_hook!(record_view_new);
/// The hook class a view's field 4 points at.
pub(super) const VIEW_SOURCE: &str = velt_common::std_hook!(record_view_source);
/// The record field that holds a view's source (std/prelude/record.velt).
const VIEW_FIELD: u32 = 4;

impl Cx<'_> {
    /// May a value of the record type `t` be a view?
    pub(super) fn record_view(&self, t: TyId) -> bool {
        !self.boxing.record_views.is_empty() && self.boxing.record_views.contains(&self.ph_rep(t))
    }

    /// Is `t` the prelude's `Record<string, any>`, the one record type a view can have?
    fn view_record_type(&mut self, t: TyId) -> bool {
        let TyKind::Adt(d, args) = self.raw_kind(t).clone() else {
            return false;
        };
        let hir::Def::Adt(a) = self.hir.def(d) else {
            return false;
        };
        let named =
            a.name == "Record" || a.name.ends_with("::Record") || a.name.ends_with(".Record");
        if !named || a.fields.first().map(|f| f.name.as_str()) != Some("entries") {
            return false;
        }
        let any = self.any_ty();
        matches!(args.as_slice(), [k, v] if matches!(self.raw_kind(*k), TyKind::Str) && *v == any)
    }

    /// Record an `AnyTo` to the type `t` (not counted in the standard library): a record type
    /// with `any` values becomes a view type.
    pub(super) fn note_record_view(&mut self, t: TyId) {
        let std = self.shapes.std_glue
            || self.cur_def.is_some_and(|d| match self.hir.def(d) {
                hir::Def::Fn(f) => f
                    .name
                    .starts_with(velt_common::std_modules::CANONICAL_PREFIX),
                _ => false,
            });
        if !std && self.view_record_type(t) {
            self.facts.record_view_targets.insert(t);
        }
    }

    /// The view types implied by this pass's facts.
    pub(super) fn close_record_views(&mut self, next: &mut Boxing) {
        if self.facts.record_view_targets.is_empty() || self.std_hook_if_loaded(VIEW_NEW).is_none()
        {
            return;
        }
        next.record_views
            .extend(self.facts.record_view_targets.iter().copied());
    }

    /// The hook class `RecordViewSource`, as a type.
    fn view_source_ty(&mut self) -> Option<TyId> {
        let d: DefId = self.std_hook_if_loaded(VIEW_SOURCE)?;
        Some(self.intern(TyKind::Adt(d, vec![])))
    }
}

impl FnLower<'_, '_> {
    /// `op` (a record object's pointer) at a place, for `field_place`.
    pub(super) fn record_place(&mut self, op: Operand) -> Place {
        match op {
            Operand::Copy(p) => p,
            op => {
                let t = self.temp(Ty::Ptr);
                self.assign(Place::local(t), Rvalue::Use(op));
                Place::local(t)
            }
        }
    }

    /// For the record of the view type `ty` at `r`: its source (field 4) when it is a view, then
    /// `body` with that source, on to the block after; nothing more for a plain record.
    fn when_view(&mut self, r: &Place, ty: TyId, body: impl FnOnce(&mut Self, Operand, Place)) {
        let word = Operand::Copy(self.field_place(r, ty, VIEW_FIELD));
        let is = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, word.clone(), cint(0, Ty::U64)),
        );
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(is, yes, join);
        self.switch_to(yes);
        let src = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(word, Ty::Ptr));
        let mirror = self.field_place(r, ty, 0);
        body(self, src, mirror);
        self.goto(join);
        self.switch_to(join);
    }

    /// The `recordView*` intrinsics (module docs). In a record type that is no view type:
    /// `recordViewIs` is `false`, `recordViewGet` a zero value (behind that `false`) and the
    /// others no code.
    pub(super) fn record_view_intrinsic(
        &mut self,
        i: Intrinsic,
        args: &[hir::Expr],
        ty: TyId,
    ) -> Operand {
        let Some(r) = args.first() else {
            return unit();
        };
        let rty = self.sub(r.ty);
        if !self.cx.record_view(rty) {
            return match i {
                Intrinsic::RecordViewIs | Intrinsic::RecordViewHas | Intrinsic::RecordViewIn => {
                    Operand::Const(Const::Bool(false), Ty::Bool)
                }
                Intrinsic::RecordViewGet => {
                    let st = self.sub(ty);
                    let vt = self.cx.ty(st);
                    self.zero_value(vt)
                }
                _ => unit(),
            };
        }
        let r = self.expr(r);
        let r = self.record_place(r);
        match i {
            Intrinsic::RecordViewIs => {
                let word = Operand::Copy(self.field_place(&r, rty, VIEW_FIELD));
                self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, word, cint(0, Ty::U64)))
            }
            // The source of a record `recordViewIs` found to be a view.
            Intrinsic::RecordViewGet
            | Intrinsic::RecordViewHas
            | Intrinsic::RecordViewIn
            | Intrinsic::RecordViewSet
            | Intrinsic::RecordViewDelete => {
                let word = Operand::Copy(self.field_place(&r, rty, VIEW_FIELD));
                let src = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(word, Ty::Ptr));
                let src = ty::RecordViewSource::arg(src);
                let kp = self.view_key_place(&args[1]);
                match i {
                    Intrinsic::RecordViewGet => {
                        // `V | undefined` at `V = any`: an option holding the `any` the hook
                        // gives (an `any` holding `undefined` reads as `undefined` too).
                        let st = self.sub(ty);
                        let any = self.cx.any_ty();
                        let at = self.cx.ty(any);
                        let got = Place::local(self.temp(at));
                        let key = ty::Str::arg(self.addr(kp));
                        hooks::record_view_get_one(self, src, key).emit_to(self, got.clone());
                        if st == any {
                            return Operand::Copy(got);
                        }
                        let some = Operand::Const(Const::Bool(true), Ty::Bool);
                        self.build_agg(st, vec![some, Operand::Copy(got)])
                    }
                    Intrinsic::RecordViewHas => {
                        let out = Place::local(self.temp(Ty::Bool));
                        let key = ty::Str::arg(self.addr(kp));
                        hooks::record_view_has_one(self, src, key).emit_to(self, out.clone());
                        Operand::Copy(out)
                    }
                    Intrinsic::RecordViewIn => {
                        let out = Place::local(self.temp(Ty::Bool));
                        let key = ty::Str::arg(self.addr(kp));
                        hooks::record_view_in_one(self, src, key).emit_to(self, out.clone());
                        Operand::Copy(out)
                    }
                    Intrinsic::RecordViewSet => {
                        let vty = self.sub(args[2].ty);
                        let vt = self.cx.ty(vty);
                        let v = self.expr(&args[2]);
                        let vp = self.record_place_agg(v, vt);
                        let v = self.addr(vp);
                        let key = ty::Str::arg(self.addr(kp));
                        hooks::record_view_set_one(self, src, key, ty::Any::moved(v))
                            .emit_std(self);
                        unit()
                    }
                    // The hook keeps the key: a new reference to it.
                    _ => {
                        let key = ty::Str::shared(kp);
                        hooks::record_view_delete_one(self, src, key).emit_std(self);
                        unit()
                    }
                }
            }
            Intrinsic::RecordViewFlush => {
                let other = self.expr(&args[1]);
                let op = self.record_place(other);
                let keys = self.field_place(&op, rty, 0);
                self.when_view(&r, rty, |lw, s, mirror| {
                    let src = ty::RecordViewSource::arg(s);
                    let m = ty::Map::arg(Operand::Copy(mirror));
                    hooks::record_view_write_all(lw, src, m, ty::Map::shared(keys)).emit_std(lw);
                });
                unit()
            }
            _ => {
                self.when_view(&r, rty, |lw, s, mirror| lw.view_resync(s, mirror, rty));
                unit()
            }
        }
    }

    /// The key operand of a keyed `recordView*` intrinsic, at a place.
    fn view_key_place(&mut self, k: &hir::Expr) -> Place {
        let kt = self.sub(k.ty);
        let vt = self.cx.ty(kt);
        let k = self.expr(k);
        self.record_place_agg(k, vt)
    }

    /// An aggregate operand `op` of VIR type `vt` at a place (to pass its address).
    fn record_place_agg(&mut self, op: Operand, vt: Ty) -> Place {
        match op {
            Operand::Copy(p) => p,
            op => {
                let t = self.temp(vt);
                self.assign(Place::local(t), Rvalue::Use(op));
                Place::local(t)
            }
        }
    }

    /// Before glue reads the entries (field 0) of the record of type `ty` at `r` directly: a
    /// view's mirror rebuilt from its source. Nothing in a type that is no view type.
    pub(super) fn record_view_sync_glue(&mut self, r: &Place, ty: TyId) {
        if !self.cx.record_view(ty) {
            return;
        }
        self.when_view(r, ty, |lw, src, mirror| lw.view_resync(src, mirror, ty));
    }

    /// The mirror (field 0, at `mirror`) of the view of type `ty` whose source is `src` replaced by
    /// a new one built from the source (`recordViewSyncAll`), the old one dropped.
    fn view_resync(&mut self, src: Operand, mirror: Place, ty: TyId) {
        let map_ty = self.cx.adt_field_tys(ty)[0];
        let fresh = Place::local(self.temp(Ty::Ptr));
        hooks::record_view_sync_all(self, ty::RecordViewSource::arg(src))
            .emit_to(self, fresh.clone());
        self.drop_glue(mirror.clone(), map_ty);
        self.assign(mirror, Rvalue::Use(Operand::Copy(fresh)));
    }

    /// The key of the view of the source the `any` at `p` holds in the runtime's view table:
    /// the object's address with bit 0 set, or for a function its identity (fn_wrappers.rs
    /// `fn_root`, any_fn_props.rs): the environment of the function its wrappers lead to, given
    /// one when it has none (a declared function's value gets the runtime's identity for its
    /// code). Each boxing of a function into `any` has its own data pointer, so that pointer
    /// would make a view per conversion (#3507).
    fn record_view_key(&mut self, p: &Place) -> Operand {
        let data = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(proj(p, Proj::Field(0)))));
        let out = Place::local(self.temp(Ty::Ptr));
        let key = self.proxy_tag(data.clone());
        self.assign(out.clone(), Rvalue::Use(key));
        // A function value (`{code, env}`), as its table says (`FN_VALUE_ID`, which a program
        // with record views gives every function type's table); a class instance that is a
        // function (a function expression with `this`, an array method's value) is keyed by its
        // address, as any other object.
        let is_fn = self.any_is_fn_value(Operand::Copy(proj(p, Proj::Field(1))));
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(is_fn, yes, join);
        self.switch_to(yes);
        let ca = self.cx.closure_agg();
        let fp = self.operand_place(data, Ty::Ptr);
        let root = self.fn_root(&proj(&fp, Proj::Deref(Ty::Agg(ca))));
        self.fn_identity_into(&root);
        let env = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&root, Proj::Field(1)))),
        );
        // A function with no identity (a generic closure without captures has no code and no
        // environment) keeps the address key.
        let has = self.non_null(env.clone());
        let named = self.new_block();
        self.branch(has, named, join);
        self.switch_to(named);
        let key = self.proxy_tag(env);
        self.assign(out.clone(), Rvalue::Use(key));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(out)
    }

    /// In `AnyTo` the view type `ty` of the `any` at `p` (owned) holding an object: the object's
    /// live view (a new reference; the `any` is released), or a new one, which becomes its live
    /// view, moved into `out`, and on to `done`.
    pub(super) fn record_view_make(&mut self, p: &Place, ty: TyId, out: &Place, done: BlockId) {
        let key = self.record_view_key(p);
        let live = Place::local(self.temp(Ty::Ptr));
        self.call_rt(Rt::ViewGet, vec![key.clone()], Some(live.clone()));
        let has = self.non_null(Operand::Copy(live.clone()));
        let (hit, miss) = (self.new_block(), self.new_block());
        self.branch(has, hit, miss);
        self.switch_to(hit);
        let kept = self.share_value(Operand::Copy(live), ty);
        let any = self.cx.any_ty();
        self.drop_glue(p.clone(), any);
        self.assign(out.clone(), Rvalue::Use(kept));
        self.goto(done);
        self.switch_to(miss);
        let arg = self.addr(p.clone());
        let src = Place::local(self.temp(Ty::Ptr));
        hooks::record_view_new(self, ty::Any::moved(arg)).emit_to(self, src.clone());
        let r = Place::local(self.temp(Ty::Ptr));
        hooks::record_view_make(self, ty::RecordViewSource::arg(Operand::Copy(src.clone())))
            .emit_to(self, r.clone());
        // The view keeps the source: its reference moves into field 4.
        let word = self.rvalue_temp(Ty::U64, Rvalue::Cast(Operand::Copy(src), Ty::U64));
        let slot = self.field_place(&r, ty, VIEW_FIELD);
        self.assign(slot, Rvalue::Use(word));
        self.call_rt(Rt::ViewPut, vec![key, Operand::Copy(r.clone())], None);
        self.assign(out.clone(), Rvalue::Use(Operand::Copy(r)));
        self.goto(done);
    }

    /// `ToDyn` of a record of the view type `t` (`data`, owned, with the descriptor `table`): a
    /// view is its source in an `any`, as in Node, where the view is the object itself. So `===`,
    /// `Object.is`, `Map`/`Set` keys and their hashes see the source (#3507), and converting the
    /// `any` back gives the same live view (`record_view_make`). The view's reference is
    /// released. A plain record is itself, and so is what `proxy_any_parts` made of a `Proxy` a
    /// variable of type `t` holds: only parts whose descriptor is still the record's own (`own`)
    /// are read as a record.
    pub(super) fn record_view_any_parts(
        &mut self,
        data: Operand,
        table: Operand,
        own: Operand,
        t: TyId,
    ) -> (Operand, Operand) {
        let Some(sty) = self.cx.view_source_ty() else {
            return (data, table);
        };
        let r = Place::local(self.copy_to_temp(data, Ty::Ptr));
        let (d, tb) = (self.temp(Ty::Ptr), self.temp(Ty::Ptr));
        self.assign(Place::local(d), Rvalue::Use(Operand::Copy(r.clone())));
        self.assign(Place::local(tb), Rvalue::Use(table));
        let mine = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(Place::local(tb)), own),
        );
        let (record, join) = (self.new_block(), self.new_block());
        self.branch(mine, record, join);
        self.switch_to(record);
        let word = Operand::Copy(self.field_place(&r, t, VIEW_FIELD));
        let is = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, word.clone(), cint(0, Ty::U64)),
        );
        let yes = self.new_block();
        self.branch(is, yes, join);
        self.switch_to(yes);
        let src = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(word, Ty::Ptr));
        let sp = self.record_place(src);
        let target = self.field_place(&sp, sty, 0);
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
        self.drop_glue(r, t);
        self.goto(join);
        self.switch_to(join);
        (
            Operand::Copy(Place::local(d)),
            Operand::Copy(Place::local(tb)),
        )
    }

    /// Before the record of the view type `ty` at `r` goes to another thread (moved: its only
    /// reference): a view becomes a plain record of its mirror, brought up to date first; its
    /// entry leaves the runtime's table, its source is released and field 4 is 0. As in Node,
    /// where what a worker gets is a structured clone. (A shared one is copied: clone glue.)
    pub(super) fn record_view_detach(&mut self, r: &Place, ty: TyId) {
        if !self.cx.record_view(ty) {
            return;
        }
        let Some(sty) = self.cx.view_source_ty() else {
            return;
        };
        let me = Operand::Copy(r.clone());
        let at = r.clone();
        self.when_view(r, ty, |lw, src, mirror| {
            lw.view_resync(src.clone(), mirror, ty);
            let sp = lw.record_place(src);
            let target = lw.field_place(&sp, sty, 0);
            let key = lw.record_view_key(&target);
            lw.call_rt(Rt::ViewRemove, vec![key, me], None);
            lw.drop_glue(sp, sty);
            let slot = lw.field_place(&at, ty, VIEW_FIELD);
            lw.assign(slot, Rvalue::Use(cint(0, Ty::U64)));
        });
    }

    /// The drop of the record of the view type `ty` at `r` (its object, about to be freed): for
    /// a view, its entry leaves the runtime's table and its source is released.
    pub(super) fn record_view_drop(&mut self, r: &Place, ty: TyId) {
        if !self.cx.record_view(ty) {
            return;
        }
        let Some(sty) = self.cx.view_source_ty() else {
            return;
        };
        let me = Operand::Copy(r.clone());
        self.when_view(r, ty, |lw, src, _| {
            let sp = lw.record_place(src);
            let target = lw.field_place(&sp, sty, 0);
            let key = lw.record_view_key(&target);
            lw.call_rt(Rt::ViewRemove, vec![key, me], None);
            lw.drop_glue(sp, sty);
        });
    }
}
