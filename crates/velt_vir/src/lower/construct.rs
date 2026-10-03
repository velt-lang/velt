//! Object construction in JavaScript's order. `new C(args)` allocates a zeroed object,
//! evaluates the arguments and runs the constructors base class first. Each class's field
//! defaults are evaluated once its base class is constructed: at the start of the constructor
//! of a class without a base, right after the `super(...)` statement of a derived constructor
//! (statements before it run first), and after the inherited constructor for a class without
//! one of its own.

use velt_sema::hir::{self, DefId, FnDef, StmtKind as S, TyId, TyKind};

use super::{ice, FnLower, ScopeKind};
use crate::vir::{Operand, Place};

/// Where a constructor body initializes fields: before top-level statement `at` (`at` may be
/// the number of statements), the own fields of each class of `classes` (base first).
pub(super) struct CtorInits {
    at: usize,
    classes: Vec<TyId>,
}

impl FnLower<'_, '_> {
    /// `new C<T>(args)`: allocate, then construct (with the type args of each class).
    pub(super) fn new_object(&mut self, ty: TyId, args: &[hir::Expr]) -> Operand {
        let ty = self.sub(ty);
        let obj = self.alloc_object(ty);
        self.construct(&obj, ty, args);
        Operand::Copy(obj)
    }

    /// Runs the construction of class type `ty` (concrete) on `obj`.
    fn construct(&mut self, obj: &Place, ty: TyId, args: &[hir::Expr]) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            ice("new of a non-class type")
        };
        let adt = self.cx.adt_def(d);
        match adt.ctor {
            Some(ctor) if self.ctor_owner(ctor) == d => {
                let cargs = self.cx.ctor_type_args(ctor, ty);
                self.call_def(ctor, cargs, vec![Operand::Copy(obj.clone())], args);
            }
            _ => {
                if let Some(base) = self.cx.bases_of(ty).first().copied() {
                    self.construct(obj, base, args);
                }
                self.init_own_fields(obj, ty);
            }
        }
    }

    fn ctor_owner(&mut self, ctor: DefId) -> DefId {
        match self.cx.fn_def(ctor).self_ty.map(|t| self.cx.kind(t)) {
            Some(TyKind::Adt(d, _)) => d,
            _ => ice("constructor without a class `this`"),
        }
    }

    /// Evaluates the defaults of the fields class type `ty` (concrete) declares itself into
    /// the object `obj`.
    fn init_own_fields(&mut self, obj: &Place, ty: TyId) {
        let TyKind::Adt(d, cargs) = self.cx.kind(ty) else {
            ice("fields of a non-class type")
        };
        let adt = self.cx.adt_def(d);
        let start = match adt.base.map(|b| self.cx.kind(b)) {
            Some(TyKind::Adt(b, _)) => self.cx.adt_def(b).fields.len(),
            _ => 0,
        };
        let saved = std::mem::replace(&mut self.targs, cargs);
        for (i, f) in adt.fields.iter().enumerate().skip(start) {
            if self.dead() {
                break;
            }
            if let Some(def) = &f.default {
                let v = self.consume(def);
                let p = self.field_place(obj, ty, i as u32);
                self.store(p, v);
            }
        }
        self.targs = saved;
    }

    /// The field initialization of constructor `def` (`None`: not a constructor). A derived
    /// constructor initializes the fields of its class and of the base classes between it and
    /// the constructor `super(...)` calls right after that call; a base class without any
    /// constructor makes `super()` the statement `Lit(Unit)`.
    pub(super) fn ctor_inits(&mut self, def: DefId, f: &FnDef) -> Option<CtorInits> {
        let ty = self.sub(f.self_ty?);
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return None;
        };
        if !matches!(self.cx.hir.def(d), hir::Def::Adt(a) if a.ctor == Some(def)) {
            return None;
        }
        let chain = self.cx.self_and_bases(ty);
        let Some(&base) = chain.get(1) else {
            return Some(CtorInits {
                at: 0,
                classes: vec![ty],
            });
        };
        let TyKind::Adt(bd, _) = self.cx.kind(base) else {
            ice("base of a non-class type")
        };
        let base_ctor = self.cx.adt_def(bd).ctor;
        let stop = base_ctor.map(|c| self.ctor_owner(c));
        let mut classes = vec![];
        for &t in &chain {
            match self.cx.kind(t) {
                TyKind::Adt(c, _) if Some(c) != stop => classes.push(t),
                _ => break,
            }
        }
        classes.reverse();
        let stmts = &f.body.block.stmts;
        let at = stmts
            .iter()
            .position(|s| is_super_stmt(s, base_ctor))
            .map_or(0, |i| i + 1);
        Some(CtorInits { at, classes })
    }

    /// Initializes the fields of `inits` if the body is at its statement `index`.
    pub(super) fn run_ctor_inits(&mut self, inits: Option<&CtorInits>, index: usize) {
        let Some(inits) = inits.filter(|i| i.at == index) else {
            return;
        };
        if self.dead() {
            return;
        }
        let this = self.info[0]
            .vir
            .unwrap_or_else(|| ice("constructor without `this`"));
        self.push_scope(ScopeKind::Temps);
        for &t in &inits.classes {
            self.init_own_fields(&Place::local(this), t);
        }
        self.pop_scope();
    }
}

/// The `super(...)` statement of a derived constructor whose base class has the constructor
/// `base_ctor` (own or inherited).
fn is_super_stmt(s: &hir::Stmt, base_ctor: Option<DefId>) -> bool {
    let S::Expr(e) = &s.kind else { return false };
    match (&e.kind, base_ctor) {
        (
            hir::ExprKind::Call {
                callee: hir::Callee::Def(c, _),
                ..
            },
            Some(b),
        ) => *c == b,
        (hir::ExprKind::Lit(hir::Lit::Unit), None) => true,
        _ => false,
    }
}
