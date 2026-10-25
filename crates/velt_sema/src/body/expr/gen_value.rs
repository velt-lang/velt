//! A generator function used as a value (`const gv = gen; gv(undefined)`) whose parameter of a
//! dynamic type has a default (`function* gen(x: any = "g")`): JavaScript applies that default
//! to a passed `undefined` too, at the call, before the generator object exists (the body runs
//! only at the first `next()`). A direct call inserts and tests the default where it is made
//! (`args.rs`, `default_on_undefined`); a call through a function value knows no defaults. So
//! such a generator is used as a value through the arrow that calls it directly,
//!
//! ```text
//! (#g0, …, #gn) => gen(#g0, …, #gn)
//! ```
//!
//! (`Class.gen` for a static generator method), checked against the generator's function type:
//! its calls apply the defaults exactly as a direct call does, at the call. The arrow captures
//! nothing, so it is checked once per generator, in the scope of the module that declares the
//! generator (or the class), and every read is that one function (`FnRef` of the arrow's
//! closure, like a named function): `gen === gen` and a `Set` holds it once, as in JS.

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::body::FnCx;
use crate::hir::{self, DefId, TyId};

impl FnCx<'_, '_> {
    /// Generator `d` (a static method of `class` when given) used as a value at `span`, when
    /// one of its parameters has a default a passed `undefined` takes (module docs); `None`
    /// otherwise, and when the context leaves a type argument of a generic generator open
    /// (`slots`: the ones known; `fn_ref_slots` reports the others).
    pub(super) fn generator_value(
        &mut self,
        d: DefId,
        class: Option<DefId>,
        slots: &[Option<TyId>],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let n = self.generator_value_params(d)?;
        let f = self.cx.fn_info(d);
        let params: Vec<TyId> = f.params.iter().map(|p| p.ty).collect();
        let (is_async, fn_module) = (f.is_async, f.module);
        let name = crate::body::defaults::fn_sig_ast(f.source?).name.clone();
        let ret = crate::body::returns::ret_of(self.cx, d, span);
        let fn_ty = self.fn_value_type(d, params, ret, is_async);
        let mut slots = slots.to_vec();
        if let Some(e) = self.hint(exp) {
            self.cx.match_ty(fn_ty, e, &mut slots);
        }
        let args = slots.into_iter().collect::<Option<Vec<TyId>>>()?;
        let key = (d, args);
        if let Some(&(c, ty)) = self.cx.generator_values.get(&key) {
            return Some(self.mk(hir::ExprKind::FnRef(c, vec![]), ty, span));
        }
        let ty = self.cx.subst(fn_ty, &key.1);
        let ty = self.fn_value_errors(d, &key.1, ty, exp, span);
        // The wrapper is checked in the scope of the module that names the generator, outside
        // any function: it is the same function wherever the generator is read.
        let (module, callee) = match class.and_then(|c| self.cx.adt(c)) {
            Some(a) => {
                let object = ast::Ident {
                    name: a.name.clone(),
                    span,
                };
                let member = ast::ExprKind::Member {
                    object: Box::new(synth(ast::ExprKind::Ident(object), span)),
                    prop: name,
                    optional: false,
                };
                (a.module, synth(member, span))
            }
            None => (fn_module, synth(ast::ExprKind::Ident(name), span)),
        };
        // The call in the arrow infers the generator's type arguments from the arrow's
        // parameters and result, which are the ones taken from the context.
        let arrow = generator_arrow(&callee, n, span);
        if key.1.iter().any(|&a| self.cx.mentions_params(a)) {
            // The type arguments name the enclosing generic function's type parameters: the
            // arrow is a closure of that function (the names it reads resolve here as they did
            // where the generator was named).
            return Some(self.closure(&arrow, Some(ty), true));
        }
        let mut fcx = crate::body::driver::detached(self.cx, module, &[]);
        let h = fcx.closure(&arrow, Some(ty), true);
        // The arrow captures nothing: one function per generator, named by every read as a
        // function is (`gen === gen`, one `Set` member), not a new closure each time.
        match h.kind {
            hir::ExprKind::Closure(c) if self.closure_captures_nothing(c) => {
                self.cx.generator_values.insert(key, (c, h.ty));
                Some(self.mk(hir::ExprKind::FnRef(c, vec![]), h.ty, span))
            }
            _ => Some(h),
        }
    }

    /// Did closure `c` (checked) capture nothing?
    fn closure_captures_nothing(&self, c: DefId) -> bool {
        matches!(&self.cx.defs[c.0 as usize], Some(crate::hir::Def::Fn(f)) if f.captures.is_empty())
    }

    /// The number of parameters of generator `d` when a call through a function value must
    /// apply one of its defaults (a parameter of a dynamic type whose default is not
    /// `undefined`); `None` for any other function, and for one with a rest parameter.
    fn generator_value_params(&mut self, d: DefId) -> Option<usize> {
        if !self.is_generator_fn(d) {
            return None;
        }
        let f = self.cx.fn_info(d);
        let src = f.source?;
        let sig = crate::body::defaults::fn_sig_ast(src);
        if sig.params.iter().any(|p| p.rest) {
            return None;
        }
        let tys: Vec<TyId> = f.params.iter().map(|p| p.ty).collect();
        let dynamic_default = sig.params.iter().zip(tys).any(|(p, ty)| {
            p.default
                .as_ref()
                .is_some_and(|e| !p.optional && !crate::body::defaults::is_undefined(e))
                && self.cx.dynamic(ty).is_some()
        });
        dynamic_default.then_some(sig.params.len())
    }
}

/// `(#g0, …, #gn) => callee(#g0, …, #gn)`.
fn generator_arrow(callee: &ast::Expr, n: usize, span: Span) -> ast::Expr {
    let name = |k: usize| ast::Ident {
        name: format!("#g{k}"),
        span,
    };
    let params = (0..n)
        .map(|k| ast::ArrowParam {
            name: name(k),
            pattern: None,
            ty: None,
            default: None,
            optional: false,
            rest: false,
        })
        .collect();
    let call = synth(
        ast::ExprKind::Call {
            callee: Box::new(callee.clone()),
            type_args: vec![],
            args: (0..n)
                .map(|k| synth(ast::ExprKind::Ident(name(k)), span))
                .collect(),
            optional: false,
        },
        span,
    );
    synth(
        ast::ExprKind::Arrow {
            type_params: vec![],
            params,
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(call)),
            is_async: false,
        },
        span,
    )
}
