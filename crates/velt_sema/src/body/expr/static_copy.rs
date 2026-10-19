//! `this.f(…)` and `this.NAME` in a subclass's copy of a static method (`collect::static_this`).
//!
//! TypeScript checks a static method's body once, with the types of the class declaring it; a
//! subclass's own static only has to be assignable to the one it overrides (TS2417). So a copy
//! types `this.f(…)` and `this.NAME` as the declaring class sees them, runs the subclass's
//! function (or reads its field), and converts the result to the declaring class's type:
//! `static create(): Dog` overriding `static create(): Animal` gives an `Animal` in the copy,
//! and `static kind(): 2` overriding `static kind(): number` a `number`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::ctx::Ctx;
use crate::hir::{self, Callee, DefId, ExprKind as H, LitValue, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `this.f(args)` in a static method whose `this` is class `this`, declared in class
    /// `declaring`.
    #[allow(clippy::too_many_arguments)] // `static_call_as` plus the declaring class
    pub(super) fn this_static_call(
        &mut self,
        this: DefId,
        declaring: DefId,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let found = |s: &mut Self, d: DefId| s.find_static(d, &prop.name).ok().map(|m| m.0.def);
        let (base, sub) = (found(self, declaring), found(self, this));
        let plain = |s: &Self, m: DefId| s.cx.fn_info(m).generics.len() == 0;
        let (Some(base), Some(sub)) = (base, sub) else {
            return self.static_call_as(this, this, prop, type_args, args, exp, span);
        };
        if this == declaring || base == sub || !plain(self, base) || !plain(self, sub) {
            return self.static_call_as(this, this, prop, type_args, args, exp, span);
        }
        // Checked as the declaring class sees `f`, then the call goes to the subclass's `f`.
        let h = self.static_call_as(declaring, declaring, prop, type_args, args, exp, span);
        let hir::Expr {
            kind:
                H::Call {
                    callee: Callee::Def(_, targs),
                    args: hargs,
                },
            ty: want_ty,
            span: _,
        } = h
        else {
            return h;
        };
        let def = self.static_def_for(sub, this);
        let cname = self
            .cx
            .adt(this)
            .map(|a| a.name.clone())
            .unwrap_or_default();
        let what = format!("`{cname}.{}`", prop.name);
        let c = self.fn_callable(def, what.clone(), span);
        if c.params.len() != hargs.len() {
            let msg = format!(
                "{what} takes {} argument(s) where the method it overrides takes {}",
                c.params.len(),
                hargs.len()
            );
            self.copy_error(msg, span);
            return self.error_expr(span);
        }
        let mut out = Vec::with_capacity(hargs.len());
        for (a, p) in hargs.into_iter().zip(&c.params) {
            out.push(self.copy_coerce(a, p.ty, &what));
        }
        self.note_async_args(def, &out);
        self.call_throws(def, &targs, c.ret, span);
        let call = H::Call {
            callee: Callee::Def(def, targs),
            args: out,
        };
        let h = self.mk(call, c.ret, span);
        self.copy_coerce(h, want_ty, &what)
    }

    /// `this.NAME` in a static method whose `this` is class `this`, declared in class
    /// `declaring`: the subclass's field as the declaring class's type.
    pub(super) fn this_static_field(
        &mut self,
        this: DefId,
        declaring: DefId,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let h = self.inherited_static_field(this, prop, want, span)?;
        if this == declaring {
            return Some(h);
        }
        let Some(g) = self.static_field_def(declaring, &prop.name) else {
            return Some(h);
        };
        crate::body::driver::ensure_global(self.cx, g);
        let ty = self.cx.global(g).expect("ICE: global").ty;
        let cname = self
            .cx
            .adt(this)
            .map(|a| a.name.clone())
            .unwrap_or_default();
        Some(self.copy_coerce(h, ty, &format!("`{cname}.{}`", prop.name)))
    }

    /// The `static` field `name` of class `d` or of its nearest base class declaring it.
    fn static_field_def(&self, d: DefId, name: &str) -> Option<DefId> {
        let mut cur = d;
        for _ in 0..64 {
            if let Some(&g) = self.cx.adt(cur)?.statics.get(name) {
                return Some(g);
            }
            cur = self.cx.class_of(self.cx.adt(cur)?.base?)?.0;
        }
        None
    }

    /// `h` (from the subclass's member `what`) converted to `ty`, the declaring class's type.
    fn copy_coerce(&mut self, mut h: hir::Expr, ty: TyId, what: &str) -> hir::Expr {
        if h.ty == ty {
            return h;
        }
        if self.cx.ty.is_float(ty) {
            // `static kind(): 2` overriding `static kind(): number`: the literal type is an
            // integer here, the overridden member's a number.
            if let TyKind::Literal(v @ LitValue::Int(..)) = self.cx.ty.kind(h.ty).clone() {
                let base = self.cx.lit_base(&v);
                h = self.try_coerce(h, base).unwrap_or_else(|h| h);
            }
            if self.cx.ty.is_int(h.ty) {
                return self.int_to_float(h, ty);
            }
        }
        match self.try_coerce(h, ty) {
            Ok(h) => h,
            Err(h) => {
                let msg = format!(
                    "{what} has type `{}`, which does not convert to `{}` of the member it overrides",
                    self.cx.display(h.ty),
                    self.cx.display(ty)
                );
                self.copy_error(msg, h.span);
                self.error_expr(h.span)
            }
        }
    }

    /// An error that only the copy of a static method for a subclass has, naming it.
    pub(super) fn copy_error(&mut self, msg: String, span: Span) {
        let note = self.copy_note();
        let mut d = Diagnostic::error(msg, span);
        if let Some(note) = note {
            d = d.with_note(note);
        }
        self.cx.error(d);
    }

    fn copy_note(&self) -> Option<String> {
        copy_note(self.cx, self.body_def?)
    }
}

/// "in `DogShelter`'s copy of `Shelter.pair` (…)" for function `def` when it is such a copy.
pub(crate) fn copy_note(cx: &Ctx, def: DefId) -> Option<String> {
    let m = *cx.static_copy_of.get(&def)?;
    let (this, declaring) = (*cx.static_this.get(&def)?, *cx.static_this.get(&m)?);
    let sub = &cx.adt(this)?.name;
    let base = &cx.adt(declaring)?.name;
    let f = &cx.fn_info(m).name;
    let f = f.rsplit(['.', ':']).next().unwrap_or(f);
    Some(format!(
        "in `{sub}`'s copy of `{base}.{f}` (`this` is `{sub}` there)"
    ))
}
