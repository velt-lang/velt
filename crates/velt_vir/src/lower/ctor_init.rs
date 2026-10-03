//! Field initializers run in JavaScript's order: a class's own initializers run once its base
//! class is constructed, before the rest of its constructor body. For `new D()` with
//! `class D extends B`: B's initializers, B's constructor body, D's initializers, D's
//! constructor body.
//!
//! Each constructor runs the initializers of its class and of the classes between it and the
//! class declaring the base constructor it calls: right after `super(...)` returns. When no
//! ancestor has a constructor, a derived class's constructor runs all of them right after its
//! `super();` statement (statements before it run first), a base class's on entry. `new C()` calls C's constructor (possibly inherited)
//! and then runs the initializers of the classes below the one declaring it.
//!
//! Those run inline at the `new`, unless the `new` is itself inside the inlined initializers of
//! the same class (initializers that construct each other in a cycle): that one calls an
//! out-of-line initializer function (`Work::Init`), so lowering terminates and ordinary
//! classes compile as before.

use velt_sema::hir::{self, DefId, LocalId, TyId, TyKind};

use super::{ice, Cx, FnLower, ScopeKind, Work};
use crate::vir::{self, Function, Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// The concrete class type when `def` is that class's own constructor.
    pub(super) fn ctor_class(&mut self, def: DefId, f: &hir::FnDef) -> Option<TyId> {
        let ty = self.sub(f.self_ty?);
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return None;
        };
        match self.cx.hir.def(d) {
            hir::Def::Adt(a) if a.ctor == Some(def) => Some(ty),
            _ => None,
        }
    }

    /// The constructor `super(...)` calls in a constructor of class `ty`, if any.
    fn base_ctor(&mut self, ty: TyId) -> Option<DefId> {
        let TyKind::Adt(d, args) = self.cx.kind(ty) else {
            return None;
        };
        let base = self.cx.adt_def(d).base?;
        let base = self.cx.subst(base, &args);
        match self.cx.kind(base) {
            TyKind::Adt(b, _) => self.cx.adt_def(b).ctor,
            _ => None,
        }
    }

    /// How many fields the class declaring constructor `ctor` has (its own and inherited
    /// ones): the initializers of those run inside `ctor`.
    pub(super) fn ctor_fields(&mut self, ctor: DefId) -> usize {
        match self.cx.fn_def(ctor).self_ty.map(|t| self.cx.kind(t)) {
            Some(TyKind::Adt(d, _)) => self.cx.adt_def(d).fields.len(),
            _ => ice("constructor without a class `this`"),
        }
    }

    /// On entry to the constructor of a class without a base class: every field initializer of
    /// the class.
    pub(super) fn ctor_entry_inits(&mut self) {
        let Some(ty) = self.ctor_self else { return };
        if self.cx.bases_of(ty).is_empty() {
            let this = self.local_place(LocalId(0));
            self.init_fields(&this, ty, 0, true);
        }
    }

    /// In the constructor of a derived class none of whose ancestors has a constructor: the
    /// index of its root-level `super();` statement (sema makes it `Lit(Unit)`), after which
    /// every field initializer runs. Statements before it (which cannot use `this`) run first,
    /// as in JavaScript.
    pub(super) fn unit_super_at(&mut self, f: &hir::FnDef) -> Option<usize> {
        let ty = self.ctor_self?;
        if self.cx.bases_of(ty).is_empty() || self.base_ctor(ty).is_some() {
            return None;
        }
        f.body.block.stmts.iter().position(|s| {
            matches!(&s.kind, hir::StmtKind::Expr(e) if matches!(e.kind, hir::ExprKind::Lit(hir::Lit::Unit)))
        })
    }

    /// After the `super();` of `unit_super_at`: every field initializer of the class (nothing
    /// has used the object yet).
    pub(super) fn unit_super_inits(&mut self) {
        let Some(ty) = self.ctor_self.filter(|_| !self.dead()) else {
            return;
        };
        let this = self.local_place(LocalId(0));
        self.push_scope(ScopeKind::Temps);
        self.init_fields(&this, ty, 0, true);
        self.pop_scope();
    }

    /// After a call of `def`: when it is the `super(...)` call of the constructor being
    /// lowered, the initializers of the fields the base constructor did not initialize.
    pub(super) fn after_super_inits(&mut self, def: DefId) {
        let Some(ty) = self.ctor_self else { return };
        if self.base_ctor(ty) != Some(def) || self.dead() {
            return;
        }
        let from = self.ctor_fields(def);
        let this = self.local_place(LocalId(0));
        self.init_fields(&this, ty, from, false);
    }

    /// Store the initializer of every field of class `ty` (concrete) from index `from` on
    /// into the object `obj` points to, evaluated in the class's type context.
    ///
    /// `fresh`: nothing has run on the object yet (its fields are still zeroed), so the values
    /// are plainly stored. Otherwise a base constructor ran first and may have assigned these
    /// fields (through an overridden method): each initializer then replaces the field like
    /// `this.f = v` does, dropping the old value (a zeroed field drops as nothing).
    pub(super) fn init_fields(&mut self, obj: &Place, ty: TyId, from: usize, fresh: bool) {
        let TyKind::Adt(d, cargs) = self.cx.kind(ty) else {
            ice("field initializers of a non-class type")
        };
        let adt = self.cx.adt_def(d);
        let ftys = self.cx.adt_field_tys(ty);
        let saved = std::mem::replace(&mut self.targs, cargs);
        for (i, f) in adt.fields.iter().enumerate().skip(from) {
            if let Some(def) = &f.default {
                let v = self.consume(def);
                let p = self.field_place(obj, ty, i as u32);
                let fty = ftys[i];
                if fresh || !self.cx.needs_drop(fty) {
                    self.store(p, v);
                } else {
                    // Store first, then drop the old value: its `dispose` may reach the object.
                    let vt = self.cx.ty(fty);
                    let old = self.copy_to_temp(Operand::Copy(p.clone()), vt);
                    self.store(p, v);
                    self.drop_glue(Place::local(old), fty);
                }
            }
        }
        self.targs = saved;
    }

    /// The initializers `new` runs for class `ty` (concrete) from field `from` on: inline, or
    /// through the out-of-line initializer when they are already being inlined here.
    pub(super) fn new_inits(&mut self, obj: &Place, ty: TyId, from: usize) {
        if !self.init_stack.contains(&ty) {
            self.init_stack.push(ty);
            self.init_fields(obj, ty, from, false);
            self.init_stack.pop();
            return;
        }
        // The out-of-line initializer throws what may be thrown here: the initializers ran
        // inline in this same context when sema checked them.
        let err = self.handler_error_ty();
        let f = self.cx.func(Work::Init(ty, err));
        let unit = self.cx.intern(TyKind::Unit);
        let argv = vec![Operand::Copy(obj.clone())];
        self.finish_call(vir::Callee::Func(f), argv, unit, err);
    }

    /// The error type an error thrown here is converted to: the innermost `try`'s, else the
    /// function's.
    fn handler_error_ty(&self) -> Option<TyId> {
        let try_ty = self.scopes.iter().rev().find_map(|s| match s.kind {
            ScopeKind::Try { ty, .. } => Some(ty),
            _ => None,
        });
        try_ty.or(self.throws)
    }

    /// `Work::Init(ty, err)`: `(this: ptr [, out: ptr])` running the initializers `new` runs
    /// for class `ty` after its constructor, throwing `err`.
    pub(super) fn build_init(cx: &mut Cx<'_>, ty: TyId, err: Option<TyId>) -> Function {
        let unit = cx.intern(TyKind::Unit);
        let from = match cx.kind(ty) {
            TyKind::Adt(d, _) => cx.adt_def(d).ctor,
            _ => ice("initializer of a non-class type"),
        };
        let mut lw = FnLower::bare(cx, vec![]);
        let from = from.map_or(0, |c| lw.ctor_fields(c));
        let this = lw.new_local(Ty::Ptr, Some("this".into()));
        let mut params = vec![Ty::Ptr];
        lw.ret_ty = Some(unit);
        lw.throws = err;
        let abi = lw.cx.ret_abi(unit, err);
        if abi.out.is_some() {
            params.push(Ty::Ptr);
            lw.out_ptr = Some(lw.new_local(Ty::Ptr, Some("ret.out".into())));
        }
        lw.init_stack.push(ty);
        lw.push_scope(ScopeKind::Block);
        lw.init_fields(&Place::local(this), ty, from, false);
        lw.emit_return(None);
        lw.pop_scope();
        let tag = err.map_or(String::new(), |e| format!("E{}_", e.0));
        let sym = format!("_Ginit_{tag}{}", lw.cx.type_symbol(ty));
        lw.finish(sym, params, abi.ret)
    }
}
