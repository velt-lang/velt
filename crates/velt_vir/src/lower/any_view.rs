//! Views of dynamic values (#1485; hir_encodings.md "Proxy and Reflect", typed proxies): in a
//! program with typed proxies, an `any` holding an object of another type converted to a
//! proxiable object type `P` (`x as VNode` of a class instance, or of an object literal with
//! more fields) is a *view* of that object, where Node keeps the one object. The view is
//! `Proxy.__view(x)` (std/proxy.vlt), a `Proxy` without traps, tagged as a typed proxy
//! (proxy_typed.rs): each field read and write goes to the object (`__get` / `__set`, the
//! value checked as the field's type, so a missing or mistyped member is a `TypeError` where
//! it is read), and nothing is copied. Converting the view back to `any` gives the object
//! itself (`FnLower::proxy_any_parts`), so `===` on dynamic values, weak map keys, `in` and
//! `instanceof` see the object, as in Node.
//!
//! An object has at most one live view: the runtime keeps a table from the object's data pointer
//! to its view (rt_abi.md "Views"), which the conversion asks first and fills when it makes a
//! view, and which the view's drop empties (`proxy_unview`). The table holds no reference, and
//! the view holds the object, so an entry lives exactly as long as its view. So converting an
//! object again while its view lives gives that view: a view's identity is its pointer, and
//! `===`, hashes and `Map` / `Set` keys of a proxiable type are the pointer code of any other
//! object type. Weak collections, which must not hold a view alive, key a view by its object
//! (weak_view.rs).
//!
//! A primitive, `null` or `undefined` stays a `TypeError`, as does an object converted to a
//! class (whose methods would run on another type's layout). A program without typed proxies
//! has no views: there `P` holds only its own layout, and the conversion stays checked by type.

use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::any::{TAG_FUNCTION, TAG_OBJECT};
use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Callee, Operand, Place, Proj, Rvalue, Ty};

/// The qualified name of the prelude function that makes a view.
const PROXY_VIEW: &str = "std/proxy::Proxy.__view";

impl FnLower<'_, '_> {
    /// In `AnyTo` to the proxiable `ty`, once the `any` at `p` (owned) holds neither a `ty` nor
    /// a proxy: an object of another type becomes a view of it (module docs), written to `out`,
    /// and on to `done`; anything else goes on in the current block.
    pub(super) fn any_view(&mut self, p: &Place, ty: TyId, out: &Place, done: vir::BlockId) {
        if self.cx.is_class(ty) || !matches!(self.cx.kind(ty), TyKind::Adt(..)) {
            return;
        }
        let Some(view) = self.cx.view_fn() else {
            return;
        };
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        let tag = self.any_tag_of(desc);
        let obj = cint(TAG_OBJECT as i128, Ty::U64);
        let fun = cint(TAG_FUNCTION as i128, Ty::U64);
        let is_obj = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag.clone(), obj));
        let is_fun = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, fun));
        let either = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitOr, is_obj, is_fun));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(either, yes, no);
        self.switch_to(yes);
        // The object's live view, if it has one (`velt_rt_view_get`): a new reference to it,
        // and the `any` released.
        let data = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(proj(p, Proj::Field(0)))));
        let live = Place::local(self.temp(Ty::Ptr));
        self.call_rt(Rt::ViewGet, vec![data.clone()], Some(live.clone()));
        let has = self.non_null(Operand::Copy(live.clone()));
        let (hit, miss) = (self.new_block(), self.new_block());
        self.branch(has, hit, miss);
        self.switch_to(hit);
        let pty = self.cx.proxy_class_ty();
        let kept = self.share_value(Operand::Copy(live), pty);
        let any = self.cx.any_ty();
        self.drop_glue(p.clone(), any);
        let tagged = self.proxy_tag(kept);
        self.assign(out.clone(), Rvalue::Use(tagged));
        self.goto(done);
        self.switch_to(miss);
        // `__view(target: any): Proxy` takes the `any` by move (an aggregate, by address).
        let f = self.cx.func_for_direct(view, vec![], self.mode);
        let arg = self.addr(p.clone());
        let r = self.temp(Ty::Ptr);
        self.call(Callee::Func(f), vec![arg], Some(Place::local(r)), false);
        let r = Operand::Copy(Place::local(r));
        self.call_rt(Rt::ViewPut, vec![data, r.clone()], None);
        let tagged = self.proxy_tag(r);
        self.assign(out.clone(), Rvalue::Use(tagged));
        self.goto(done);
        self.switch_to(no);
    }

    /// The `Proxy` class and its type.
    fn proxy_def(&mut self) -> (DefId, TyId) {
        let pty = self.cx.proxy_class_ty();
        match self.cx.kind(pty) {
            TyKind::Adt(d, _) => (d, pty),
            _ => super::ice("the `Proxy` class is not a class"),
        }
    }

    /// In the drop of the class `d` object at `this` (the untagged pointer), before its fields
    /// drop: a view of a `Proxy`, in a program with views, leaves the runtime's table of live
    /// views (`velt_rt_view_remove`), read from the fields themselves (`#view`, `#target`).
    pub(in crate::lower) fn proxy_unview(&mut self, this: Operand, d: DefId) {
        if self.cx.boxing.proxy_class != Some(d) || self.cx.view_fn().is_none() {
            return;
        }
        let pty = self.cx.proxy_class_ty();
        let view = self.proxy_hot_place(&this, pty, self.proxy_field_index(d, "#view"));
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(Operand::Copy(view), yes, join);
        self.switch_to(yes);
        let target = self.proxy_hot_place(&this, pty, self.proxy_field_index(d, "#target"));
        let data = Operand::Copy(proj(&target, Proj::Field(0)));
        self.call_rt(Rt::ViewRemove, vec![data, this], None);
        self.goto(join);
        self.switch_to(join);
    }

    /// The index of the `Proxy` class's field `name`.
    fn proxy_field_index(&self, pd: DefId, name: &str) -> u32 {
        let fields = &self.cx.adt_def(pd).fields;
        match fields.iter().position(|f| f.name == name) {
            Some(i) => i as u32,
            None => super::ice(format_args!("the `Proxy` class without `{name}`")),
        }
    }

    /// `obj.__isView()` of the (untagged, borrowed) `Proxy` object at `obj`.
    fn proxy_is_view(&mut self, pd: DefId, obj: &Place) -> Operand {
        let m = self.class_method(pd, "__isView");
        let f = self.cx.func_for_direct(m, vec![], self.mode);
        let b = self.temp(Ty::Bool);
        let this = Operand::Copy(obj.clone());
        self.call(Callee::Func(f), vec![this], Some(Place::local(b)), false);
        Operand::Copy(Place::local(b))
    }

    /// `obj.__target()` of the (untagged, borrowed) `Proxy` object at `obj`: an owned `any`.
    fn proxy_view_target(&mut self, pd: DefId, obj: &Place) -> Place {
        let m = self.class_method(pd, "__target");
        let f = self.cx.func_for_direct(m, vec![], self.mode);
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let held = Place::local(self.temp(avt));
        let abi = self.cx.ret_abi(any, None);
        let this = Operand::Copy(obj.clone());
        match abi.out {
            Some(_) => {
                let a = self.addr(held.clone());
                self.call(Callee::Func(f), vec![this, a], None, false);
            }
            None => self.call(Callee::Func(f), vec![this], Some(held.clone()), false),
        }
        held
    }

    /// The `(data, table)` of the `any` for the untagged `Proxy` object `u` (owned): the
    /// object itself with the `Proxy` table `table`, or, for a view, the `any` it views (a
    /// new reference to it; the view is released).
    pub(super) fn proxy_or_view_parts(&mut self, u: Operand, table: Operand) -> (Operand, Operand) {
        let (d, t) = (self.temp(Ty::Ptr), self.temp(Ty::Ptr));
        if self.cx.view_fn().is_none() {
            self.assign(Place::local(d), Rvalue::Use(u));
            self.assign(Place::local(t), Rvalue::Use(table));
            return (
                Operand::Copy(Place::local(d)),
                Operand::Copy(Place::local(t)),
            );
        }
        let (pd, pty) = self.proxy_def();
        let obj = Place::local(self.temp(Ty::Ptr));
        self.assign(obj.clone(), Rvalue::Use(u));
        let b = self.proxy_is_view(pd, &obj);
        let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(b, yes, no);
        self.switch_to(yes);
        let held = self.proxy_view_target(pd, &obj);
        self.drop_glue(obj.clone(), pty);
        let hd = Operand::Copy(proj(&held, Proj::Field(0)));
        let ht = Operand::Copy(proj(&held, Proj::Field(1)));
        self.assign(Place::local(d), Rvalue::Use(hd));
        self.assign(Place::local(t), Rvalue::Use(ht));
        self.goto(join);
        self.switch_to(no);
        self.assign(Place::local(d), Rvalue::Use(Operand::Copy(obj)));
        self.assign(Place::local(t), Rvalue::Use(table));
        self.goto(join);
        self.switch_to(join);
        (
            Operand::Copy(Place::local(d)),
            Operand::Copy(Place::local(t)),
        )
    }
}

impl super::Cx<'_> {
    /// The prelude's `Proxy.__view`, in a program that has loaded `Proxy`.
    pub(super) fn view_fn(&mut self) -> Option<DefId> {
        if let Some(f) = self.view_fn {
            return f;
        }
        let found = self.hir.defs.iter().position(|d| match d {
            hir::Def::Fn(f) => f.name == PROXY_VIEW && f.params.len() == 1,
            _ => false,
        });
        let f = found.map(|i| DefId(i as u32));
        self.view_fn = Some(f);
        f
    }
}
