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
use crate::hir::{TyId, TyKind};
use crate::resolve::TyEnv;

/// The fields of a construct signature's type, in order.
pub(crate) const NEW: &str = "#new";
pub(crate) const IS: &str = "#is";
pub(crate) const NAME: &str = "#name";

/// Is `name` a field of a construct signature's type?
pub(crate) fn is_ctor_field(name: &str) -> bool {
    [NEW, IS, NAME].contains(&name)
}

/// The function type forms Velt parses but doesn't support yet (#943): its error and a note.
pub(crate) fn unsupported_fn_type(kind: &ast::TypeExprKind) -> (&'static str, &'static str) {
    match kind {
        ast::TypeExprKind::Construct(_) => (
            "generic construct signatures (`new <T>(x: T) => C<T>`) are not supported yet",
            "give the type parameters values (`new (x: string) => C<string>`)",
        ),
        _ => (
            "generic function types (`<T>(x: T) => R`) are not supported yet",
            "a function value has one type; declare a generic `function` and call it, or give the type parameters values (`(x: string) => R`)",
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
}
