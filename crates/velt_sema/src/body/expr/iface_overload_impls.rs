//! An object literal's method that implements an overloaded interface method
//! (`crate::iface_overloads`): as in TypeScript, it must fit every signature (TS2322 /
//! TS2416). Each signature's parameter converts to the implementation's, a parameter the
//! implementation requires is required in every signature, and the result is `any` or a type
//! every signature's result takes. The method that takes every form fixes only the slot (its
//! parameters and result are `any` where the signatures differ), so this is the check.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::iface_object::MethodSig;
use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};
use crate::overloads::sig_of;

/// A parameter of the implementation as written: its name and whether a call must pass it.
struct Written {
    name: String,
    required: bool,
}

impl FnCx<'_, '_> {
    /// Implementation `f` (a function value, written as `v`) of method `name` of an interface
    /// with `methods`, or an error value, reported, when it doesn't fit one of `name`'s
    /// signatures (`name#k`).
    pub(super) fn overload_impl_results(
        &mut self,
        methods: &[MethodSig],
        name: &str,
        (f, v): (hir::Expr, Option<&ast::Expr>),
        span: Span,
    ) -> hir::Expr {
        let TyKind::FnPtr {
            params, ret, rest, ..
        } = self.cx.ty.kind(f.ty).clone()
        else {
            return f;
        };
        let sigs: Vec<&MethodSig> = (methods.iter())
            .filter(|m| sig_of(&m.name) == Some(name))
            .collect();
        if sigs.is_empty() {
            return f;
        }
        // Each call passes the slot's arguments, `undefined` for those it leaves out, where Node
        // passes only the call's: a rest parameter would see them.
        if rest || v.is_some_and(written_rest) {
            self.cx.error(
                Diagnostic::error(
                    format!("an implementation of the overloaded method `{name}` with a rest parameter is not supported yet (#964)"),
                    span,
                )
                .with_note("its rest parameter would hold an `undefined` for each argument a call leaves out, where Node passes only the call's arguments; declare the parameters (`name: string, definition?: any`)"),
            );
            return self.error_expr(span);
        }
        // The written parameters (a `this` parameter aside), matched to the function's last ones
        // (its receiver comes first when it uses `this`). A function given by reference has no
        // written parameters: one whose type doesn't hold `undefined` is required.
        let written = v.map(written_params).unwrap_or_default();
        let skip = params.len().saturating_sub(written.len());
        let own: Vec<(TyId, Option<&Written>)> = (params.iter().enumerate())
            .skip(match written.is_empty() {
                true => 0,
                false => skip,
            })
            .map(|(i, t)| (*t, written.get(i.wrapping_sub(skip))))
            .collect();
        for sig in sigs {
            if let Some(why) = self.misfit(sig, &own, ret) {
                self.cx.error(
                    Diagnostic::error(
                        format!("method `{name}` does not fit every signature of the interface's `{name}`: {why}"),
                        span,
                    )
                    .with_note("an implementation of an overloaded method takes every signature's arguments and returns a type every signature's result takes, or `any`, as in TypeScript (TS2416)"),
                );
                return self.error_expr(span);
            }
        }
        f
    }

    /// Why the implementation (`own` parameters, result `ret`) doesn't fit signature `sig`.
    fn misfit(
        &mut self,
        sig: &MethodSig,
        own: &[(TyId, Option<&Written>)],
        ret: TyId,
    ) -> Option<String> {
        for (i, (t, w)) in own.iter().enumerate() {
            let pname = w.map_or_else(|| format!("{}", i + 1), |w| format!("`{}`", w.name));
            let required = match w {
                Some(w) => w.required,
                None => !self.holds_undefined(*t),
            };
            let Some(p) = sig.params.get(i) else {
                if required {
                    return Some(format!(
                        "its parameter {pname} is required, and a signature leaves it out"
                    ));
                }
                continue;
            };
            if required && p.default.is_some() {
                return Some(format!(
                    "its parameter {pname} is required, and a signature makes it optional"
                ));
            }
            if self.cx.dynamic(*t).is_some() || self.cx.ty.has_error(*t) {
                continue;
            }
            if !self.converts_to(p.ty, *t) {
                let (have, want) = (self.cx.display(*t), self.cx.display(p.ty));
                return Some(format!(
                    "its parameter {pname} (`{have}`) does not take the signature's `{want}`"
                ));
            }
        }
        if self.cx.dynamic(ret).is_some() || self.cx.ty.has_error(ret) {
            return None;
        }
        if !self.converts_to(ret, sig.ret) {
            let (r, w) = (self.cx.display(ret), self.cx.display(sig.ret));
            return Some(format!("its result `{r}` does not convert to `{w}`"));
        }
        None
    }
}

/// The parameters of a method written as an arrow or function expression.
fn written_params(v: &ast::Expr) -> Vec<Written> {
    match &v.kind {
        ast::ExprKind::Paren(x) => written_params(x),
        ast::ExprKind::Arrow { params, .. } => (params.iter())
            .filter(|p| p.name.name != ast::METHOD_THIS)
            .map(|p| Written {
                name: p.name.name.clone(),
                required: !p.optional && p.default.is_none() && !p.rest,
            })
            .collect(),
        ast::ExprKind::Function(d) => (d.sig.params.iter())
            .filter(|p| p.name.name != ast::METHOD_THIS)
            .map(|p| Written {
                name: p.name.name.clone(),
                required: !p.optional && p.default.is_none() && !p.rest,
            })
            .collect(),
        _ => vec![],
    }
}

impl FnCx<'_, '_> {
    /// Can a parameter of type `t` take the `undefined` a call that leaves it out passes?
    fn holds_undefined(&self, t: TyId) -> bool {
        self.cx.dynamic(t).is_some()
            || self.cx.ty.has_error(t)
            || (self.cx.ty.nothing_of(t)).is_some_and(|n| hir::Nothing::Undefined.within(n))
    }
}

/// Does the method written as `v` have a rest parameter?
fn written_rest(v: &ast::Expr) -> bool {
    match &v.kind {
        ast::ExprKind::Paren(x) => written_rest(x),
        ast::ExprKind::Arrow { params, .. } => params.iter().any(|p| p.rest),
        ast::ExprKind::Function(d) => d.sig.params.iter().any(|p| p.rest),
        _ => false,
    }
}
