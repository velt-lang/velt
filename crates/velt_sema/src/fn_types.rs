//! Function types (`(a: A, ...xs: T[]) => R throws E`): construct signatures and the forms not
//! supported yet. Plain function types are resolved by `Ctx::resolve_fn_type`
//! (`throws::fn_types`, which also gives a type written without `throws` its inferred error
//! type).
//!
//! A **construct signature** (`new (buf: ArrayBuffer) => ArrayBufferView`, #943) is the type of
//! a class used as a value. It is an anonymous object type of three readonly hidden fields
//! (their names start with `#`, like a callable object's function, `crate::callables`), which
//! only sema writes ([`Ctx::ctor_type`]):
//! - `#new`, the function type of the signature: a closure that constructs an instance
//!   (`(b) => new Uint8Array(b)`), so `new C(buf)` calls it;
//! - `#is: (v: unknown) => boolean`, which tests a value with `instanceof` of the class, so
//!   `v instanceof C` calls it;
//! - `#name`, the class's name (`C.name`).
//!
//! A class named where a value of such a type is expected becomes that object
//! (`body::expr::ctor_values`). Diagnostics show the type as written (`new (...) => T`).

use velt_syntax::ast;

use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{DefId, TyId, TyKind};
use crate::resolve::TyEnv;

/// The fields of a construct signature's type, in order.
pub(crate) const NEW: &str = "#new";
pub(crate) const IS: &str = "#is";
pub(crate) const NAME: &str = "#name";

/// Is `name` a field of a construct signature's type?
pub(crate) fn is_ctor_field(name: &str) -> bool {
    [NEW, IS, NAME].contains(&name)
}

/// The function type forms Velt parses but doesn't support yet (#943): its error and a note. (A
/// constant of a generic function type is a generic function, `generic_arrows::typed`.)
pub(crate) fn unsupported_fn_type(kind: &ast::TypeExprKind) -> (&'static str, &'static str) {
    match kind {
        ast::TypeExprKind::Construct(_) => (
            "generic construct signatures (`new <T>(x: T) => C<T>`) are not supported yet",
            "give the type parameters values (`new (x: string) => C<string>`)",
        ),
        _ => (
            "generic function types (`<T>(x: T) => R`) are not supported yet",
            "a value of this type stays generic, which only a `const` initialized with an arrow function or a name can be (`const id: Id = (x) => x;`, #943); elsewhere declare a generic `function` and call it, or give the type parameters values (`(x: string) => R`)",
        ),
    }
}

impl Ctx<'_> {
    /// `new (params) => R` (`inner` is the function type after `new`).
    pub(crate) fn resolve_construct(
        &mut self,
        inner: &ast::TypeExpr,
        t: &ast::TypeExpr,
        env: &TyEnv,
    ) -> TyId {
        if !matches!(inner.kind, ast::TypeExprKind::Function { .. }) {
            let (msg, note) = unsupported_fn_type(&t.kind);
            self.error(velt_common::Diagnostic::error(msg, t.span).with_note(note));
            return self.ty.error;
        }
        let f = self.resolve_type(inner, env);
        if f == self.ty.error {
            return f;
        }
        self.ctor_type(f, t.span, env.module)
    }

    /// The type of a construct signature whose function type is `f` (module docs).
    pub(crate) fn ctor_type(&mut self, f: TyId, span: velt_common::Span, module: usize) -> TyId {
        let unknown = ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ast::Ident {
                    name: "unknown".into(),
                    span,
                }],
                args: vec![],
            },
            span,
        };
        let unknown = self.resolve_type(&unknown, &TyEnv::new(module, &[]));
        let is = self.ty.intern(TyKind::FnPtr {
            params: vec![unknown],
            ret: self.ty.bool_,
            throws: self.ty.never,
            rest: false,
        });
        let field = |name: &str, ty| ShapeField {
            name: name.into(),
            ty,
            readonly: true,
            optional: false,
        };
        let fields = [field(NEW, f), field(IS, is), field(NAME, self.ty.str_)];
        self.anon_type_with(&fields, module)
    }

    /// The function type of construct signature type `t` (`None`: `t` is not one).
    pub(crate) fn ctor_sig(&mut self, t: TyId) -> Option<TyId> {
        let (own, args) = self.ctor_sig_own(t)?;
        let ty = self.subst(own, &args);
        matches!(self.ty.kind(ty), TyKind::FnPtr { .. }).then_some(ty)
    }

    /// Construct signature type `t` as written (`new (n: number) => C`), with `names` the type
    /// parameters in scope.
    pub(crate) fn display_ctor(&self, t: TyId, names: &[String]) -> String {
        let (f, args) = self.ctor_sig_own(t).expect("ICE: construct signature");
        let bound: Vec<String> = args.iter().map(|t| self.display_in(*t, names)).collect();
        format!("new {}", self.display_in(f, &bound))
    }

    /// The function type of construct signature type `t` over its def's own type parameters,
    /// and `t`'s type arguments.
    pub(crate) fn ctor_sig_own(&self, t: TyId) -> Option<(TyId, Vec<TyId>)> {
        let TyKind::Adt(d, args) = self.ty.kind(t) else {
            return None;
        };
        let a = self.adt(*d)?;
        if a.kind != crate::hir::AdtKind::Anon || a.fields.len() != 3 {
            return None;
        }
        let names: Vec<&str> = a.fields.iter().map(|f| f.name.as_str()).collect();
        if names != [NEW, IS, NAME] {
            return None;
        }
        Some((a.fields[0].ty, args.clone()))
    }

    /// Are construct signature types `a` and `b` one type but for the error-set variables of
    /// their functions (`throws::vars`)? Then those are unified, and a value of one is the
    /// value of the other: the class's one object. Two written over type parameters
    /// (`new (n: number) => T` in two places) are generic anonymous types of their own, whose
    /// fields the variables' comparisons don't look into.
    pub(crate) fn unify_ctor_vars(&mut self, a: TyId, b: TyId) -> bool {
        if a == b {
            return false;
        }
        match (self.ctor_sig(a), self.ctor_sig(b)) {
            (Some(fa), Some(fb)) => self.unify_err_vars(fa, fb),
            _ => false,
        }
    }

    /// Is `t` a construct signature type (a class value's)?
    pub(crate) fn is_ctor_type(&self, t: TyId) -> bool {
        self.ctor_sig_own(self.ty.core(t).unwrap_or(t)).is_some()
    }

    /// Forgets class values made by a check that was undone (`body::recheck`): nothing reads
    /// their constants, which leave the module state as `()`; the next check makes them again.
    pub(crate) fn drop_class_values(&mut self, dropped: Vec<((DefId, TyId), DefId)>) {
        let unit = self.ty.unit;
        for (_, d) in dropped {
            if let DefInfo::Global(g) = &mut self.info[d.0 as usize] {
                g.init = Some(crate::hir::Expr {
                    kind: crate::hir::ExprKind::Lit(crate::hir::Lit::Unit),
                    ty: unit,
                    span: velt_common::Span::DUMMY,
                });
                g.ty = unit;
                g.stored = false;
            }
        }
    }
}
