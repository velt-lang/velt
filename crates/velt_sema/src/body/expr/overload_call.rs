//! Calls of TypeScript-form overloads (`crate::overloads`): `f(args)`, `o.m(args)` and
//! `C.m(args)` try the signatures `f#0`, `f#1`, ... in order and call the first that accepts
//! the arguments, as `tsc` does; when none does the error is TS2769, with why each one failed.
//! In a signature's own body the name is the implementation. Each try is checked and rolled
//! back (`recheck::Mark`), so only a call of an overloaded name pays for it.
//!
//! A signature's body returns the implementation's result as its own type. When that is
//! narrower (a member of the implementation's union result), the conversion tests the member
//! and panics on another one: `checked_narrow`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, LocalKind, Want};
use crate::ctx::Item;
use crate::hir::{self, ExprKind as H, PatKind as P, TyId, UseMode};
use crate::overloads::{sig_name, sig_of};

/// Why each signature rejected a call (its first error), in order.
type Misfits = Vec<String>;

impl FnCx<'_, '_> {
    /// Is the body being checked signature `name#k` of an overload set, whose calls of `name`
    /// are its implementation?
    pub(crate) fn in_signature_of(&self, name: &str) -> bool {
        self.signature_body()
            .is_some_and(|own| sig_of(&own) == Some(name))
    }

    /// The name of the signature whose body is being checked.
    pub(crate) fn signature_body(&self) -> Option<String> {
        if !self.cx.has_overloads || !self.outer.is_empty() {
            return None;
        }
        let d = self.body_def?;
        let src = self.cx.fn_info(d).source?;
        let name = &crate::body::defaults::fn_sig_ast(src).name.name;
        sig_of(name).is_some().then(|| name.clone())
    }

    /// `name(args)` where `name` is an overloaded function: the call of the first signature
    /// that accepts it (`None`: `name` has no signatures here).
    pub(crate) fn overloaded_call(
        &mut self,
        id: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        if self.in_signature_of(&id.name) {
            return None;
        }
        let mut sigs = vec![];
        loop {
            let name = sig_name(&id.name, sigs.len());
            match self.cx.lookup_item_at(self.module, &name, id.span) {
                Some(Item::Def(_)) => sigs.push(ast::Ident {
                    name,
                    span: id.span,
                }),
                _ => break,
            }
        }
        if sigs.is_empty() {
            return None;
        }
        let picked = self.pick_signature(&id.name, sigs.len(), |s, k| {
            s.named_call(&sigs[k], type_args, args, exp, span)
        });
        Some(match picked {
            Ok(k) => self.named_call(&sigs[k], type_args, args, exp, span),
            Err(misfits) => self.no_overload(&id.name, misfits, args, span),
        })
    }

    /// `recv.name(args)` where the receiver's type has signatures `name#k`: `Ok` with the call
    /// of the first that accepts it, `Err` with the receiver when there are none.
    pub(crate) fn overloaded_method_call(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.cx.overloaded_methods.contains(&prop.name) || self.in_signature_of(&prop.name) {
            return Err(recv);
        }
        let mut sigs = vec![];
        loop {
            let name = sig_name(&prop.name, sigs.len());
            if !self.method_exists(recv.ty, &name) {
                break;
            }
            sigs.push(ast::Ident {
                name,
                span: prop.span,
            });
        }
        if sigs.is_empty() {
            return Err(recv);
        }
        let picked = self.pick_signature(&prop.name, sigs.len(), |s, k| {
            s.method_call_at(recv.clone(), &sigs[k], type_args, args, exp, span, false)
        });
        Ok(match picked {
            Ok(k) => self.method_call_at(recv, &sigs[k], type_args, args, exp, span, false),
            Err(misfits) => self.no_overload(&prop.name, misfits, args, span),
        })
    }

    /// `C.name(args)` of an overloaded static method of class `d` (`None`: not overloaded).
    #[allow(clippy::too_many_arguments)] // `static_call_as` plus the class
    pub(crate) fn overloaded_static_call(
        &mut self,
        d: hir::DefId,
        this_class: hir::DefId,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        if !self.cx.overloaded_methods.contains(&prop.name) || self.in_signature_of(&prop.name) {
            return None;
        }
        let mut sigs = vec![];
        loop {
            let name = sig_name(&prop.name, sigs.len());
            if self.find_static(d, &name).is_err() {
                break;
            }
            sigs.push(ast::Ident {
                name,
                span: prop.span,
            });
        }
        if sigs.is_empty() {
            return None;
        }
        let picked = self.pick_signature(&prop.name, sigs.len(), |s, k| {
            s.static_call_as(d, this_class, &sigs[k], type_args, args, exp, span)
        });
        Some(match picked {
            Ok(k) => self.static_call_as(d, this_class, &sigs[k], type_args, args, exp, span),
            Err(misfits) => self.no_overload(&prop.name, misfits, args, span),
        })
    }

    /// The first of `n` signatures whose call (`call(k)`, checked as a try and rolled back)
    /// reports no error, or each one's first error.
    fn pick_signature(
        &mut self,
        name: &str,
        n: usize,
        mut call: impl FnMut(&mut Self, usize) -> hir::Expr,
    ) -> Result<usize, Misfits> {
        let mut misfits = vec![];
        for k in 0..n {
            let mark = crate::body::recheck::Mark::here(self.cx);
            let frames = self.trial_frames();
            let diags = self.cx.diags.len();
            let temps = self.call_temps.len();
            call(self, k);
            let first = self.cx.diags[diags..]
                .iter()
                .find(|d| d.is_error())
                .map(|d| {
                    let why = match d.notes.first() {
                        Some(note) => format!("{} ({note})", d.message),
                        None => d.message.clone(),
                    };
                    why.replace(&sig_name(name, k), name)
                });
            mark.rollback(self.cx);
            self.cx.diags.truncate(diags);
            self.call_temps.truncate(temps);
            self.restore_trial_frames(frames);
            match first {
                None => return Ok(k),
                Some(m) => misfits.push(m),
            }
        }
        Err(misfits)
    }

    /// TS2769: no signature of `name` accepts the call.
    fn no_overload(
        &mut self,
        name: &str,
        misfits: Misfits,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let n = misfits.len();
        let mut d = Diagnostic::error(format!("no overload of `{name}` matches this call"), span);
        for (k, why) in misfits.into_iter().enumerate() {
            d = d.with_note(format!("overload {} of {n}: {why}", k + 1));
        }
        self.cx.error(d);
        self.check_args_loose(args);
        self.error_expr(span)
    }

    /// `return e;` in a signature's body: the implementation's result as the signature's
    /// result type `ret`, tested when it is a wider union (`checked_narrow`).
    pub(crate) fn signature_result(&mut self, e: &ast::Expr, ret: TyId) -> hir::Expr {
        let h = self.expr(e, None, Want::Move);
        if self.cx.ty.has_error(h.ty) {
            return h;
        }
        match self.try_coerce(h, ret) {
            Ok(h) => h,
            Err(h) => self.checked_narrow(h, ret),
        }
    }

    /// `h` (a union, or nullable) as `target`: a `match` on its member, converting each member
    /// that is a `target` and panicking on the others. An error when no member is one.
    fn checked_narrow(&mut self, h: hir::Expr, target: TyId) -> hir::Expr {
        let span = h.span;
        let sty = h.ty;
        let (inner, nullable) = match self.cx.ty.opt_payload(sty) {
            Some(p) => (p, true),
            None => (sty, false),
        };
        let union = self.cx.union_def(inner).map(|(def, _)| def);
        let members = match union {
            Some(_) => self.cx.union_members(inner).unwrap_or_default(),
            None => vec![inner],
        };
        let name = self
            .signature_body()
            .and_then(|s| sig_of(&s).map(str::to_string))
            .unwrap_or_default();
        let target_name = self.cx.display(target);
        let mut arms = vec![];
        let mut fits = false;
        let panic_arm = |s: &mut Self, member: String| {
            let msg = format!(
                "overload `{name}` returned a `{member}` where its signature promises a `{target_name}`"
            );
            let msg = s.str_lit(&msg, span);
            s.intrinsic(hir::Intrinsic::Panic, vec![msg], s.cx.ty.never, span)
        };
        if nullable {
            let null = self.mk(H::Lit(hir::Lit::Null), sty, span);
            let body = match self.try_coerce(null, target) {
                Ok(b) => {
                    fits = true;
                    b
                }
                Err(_) => panic_arm(self, "null".to_string()),
            };
            arms.push(hir::Arm {
                pat: self.pat(P::None, sty, span),
                guard: None,
                body,
            });
        }
        for (v, m) in members.iter().enumerate() {
            let mode = match self.cx.is_copy(*m) {
                true => UseMode::Copy,
                false => UseMode::Move,
            };
            let b = self.new_local("<result>", *m, false, span, LocalKind::Bind);
            let bind = self.pat(P::Binding(b, mode), *m, span);
            let mut pat = match union {
                Some(def) => self.pat(
                    P::Variant {
                        def,
                        variant: v as u32,
                        args: vec![bind],
                    },
                    inner,
                    span,
                ),
                None => bind,
            };
            if nullable {
                pat = self.pat(P::Some(Box::new(pat)), sty, span);
            }
            let read = self.mk(H::Local(b, mode), *m, span);
            let body = match self.try_coerce(read, target) {
                Ok(b) => {
                    fits = true;
                    b
                }
                Err(_) => {
                    let member = self.cx.display(*m);
                    panic_arm(self, member)
                }
            };
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        if !fits {
            let found = self.cx.display(sty);
            self.cx.error(
                Diagnostic::error("mismatched types", span)
                    .with_note(format!("expected {target_name}, found {found}")),
            );
            return self.error_expr(span);
        }
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, target, span)
    }
}
