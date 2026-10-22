//! `for (const k in o) body` in a program that loads `Proxy`, where `o` is an `any` or a value of
//! a class or object type that may be a typed proxy (issue #823; docs/reference/types.md
//! "Proxy and Reflect"). Node evaluates `o` once, lists a proxy's keys with its `ownKeys` trap
//! and runs its `getOwnPropertyDescriptor` trap just before each key's turn. So:
//!
//! ```text
//! { const <forin#n> = o;
//!   const <forinp#n> = <forin#n> instanceof Proxy;   // a typed value: `Intrinsic::IsTypedProxy`
//!   for (const k of <forinp#n> ? __anyForInKeys(<forin#n>) : Object.keys(<forin#n>)) {
//!     if (<forinp#n> && !__anyForInHas(<forin#n>, k)) continue;
//!     body } }
//! ```
//!
//! A plain value takes the loop it has without proxies (its type's `Object.keys`) and one test of
//! a hidden `boolean` per key; only a proxy is converted to `any` and asked about each key.

use velt_syntax::ast::{self, ExprKind as E};

use super::consume::Synth;
use super::{FnCx, Want};
use crate::hir::{self, Intrinsic, StmtKind as S};

/// The prefix of the hidden constant holding the loop's object: `Object.keys` of it is the plain
/// value's own (the typed proxy hook of `proxy.rs` skips it).
pub(crate) const FOR_IN_OBJECT: &str = "<forin#";

impl FnCx<'_, '_> {
    /// The loop above for `for (kind k in iter) body` (`iter` the parser's `Object.keys(o)`);
    /// `false` (nothing written) for any other loop.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn for_in_proxy(
        &mut self,
        kind: ast::VarKind,
        pattern: &ast::Pattern,
        iter: &ast::Expr,
        body: &ast::Block,
        label: Option<&ast::Ident>,
        span: velt_common::Span,
        out: &mut Vec<hir::Stmt>,
    ) -> bool {
        let E::Call { callee, args, .. } = &iter.kind else {
            return false;
        };
        let (E::Member { prop, .. }, [obj], ast::PatternKind::Ident(k)) =
            (&callee.kind, args.as_slice(), &pattern.kind)
        else {
            return false;
        };
        if callee.span.lo != callee.span.hi || prop.name != "keys" || !self.uses_proxy() {
            return false;
        }
        let Some(ty) = self.trial_type(obj) else {
            return false;
        };
        let any = self.cx.dynamic(ty) == Some(crate::any::Dynamic::Any);
        if !any && !self.may_be_typed_proxy(ty) {
            return false;
        }
        if any {
            // The test reads the value's members through its descriptor.
            self.note_dyn_members();
        }
        let n = self.f.locals.len();
        let syn = Synth::new(span, n);
        let (object, is_proxy) = (format!("{FOR_IN_OBJECT}{n}>"), format!("<forinp#{n}>"));
        let bool_ = self.cx.ty.bool_;
        self.push_scope_until(span.hi);
        let mut stmts = vec![];
        let call = |f: &str, args: Vec<ast::Expr>| syn.call(syn.name(f), args);
        let keys_name = format!("<forink#{n}>");
        let decl = |name: &str, ty: Option<ast::TypeExpr>, init: ast::Expr| {
            syn.stmt(ast::StmtKind::Var(ast::VarDecl {
                kind: ast::VarKind::Const,
                pattern: syn.ident_pat(name),
                ty,
                init: Some(init),
                span: syn.at,
            }))
        };
        let operand = if any {
            // The `any` itself, held for the loop.
            let h = self.expr(obj, None, Want::Borrow);
            let init = self.share_if_place(h);
            self.hidden_local(syn.ident(&object), init, false, &mut stmts);
            let e = syn.expr(E::InstanceOf {
                expr: Box::new(syn.name(&object)),
                ty: syn.named_type("Proxy"),
            });
            let test = self.expr(&e, None, Want::Move);
            self.hidden_local(syn.ident(&is_proxy), test, false, &mut stmts);
            syn.name(&object)
        } else {
            // A typed value is read where it is (a call's result is held once): only a proxy
            // is taken as an `any` (`__proxyAny`), so the value's type stays uncounted.
            let operand = match super::expr::object_keys::reevaluable(obj) {
                true => obj.clone(),
                false => {
                    let held = format!("<forino#{n}>");
                    let h = self.expr(obj, None, Want::Borrow);
                    let init = self.share_if_place(h);
                    self.hidden_local(syn.ident(&held), init, false, &mut stmts);
                    syn.name(&held)
                }
            };
            let read = self.expr(&operand, None, Want::Borrow);
            let test = self.intrinsic(Intrinsic::IsTypedProxy, vec![read], bool_, obj.span);
            self.hidden_local(syn.ident(&is_proxy), test, false, &mut stmts);
            // `IsTypedProxy(o) ? __proxyAny(o) : undefined`, tested on `o` itself so that the
            // conversion to `any` is left out where `o`'s type cannot hold a proxy.
            let read = self.expr(&operand, None, Want::Borrow);
            let cond = self.intrinsic(Intrinsic::IsTypedProxy, vec![read], bool_, obj.span);
            let then = self.prelude_fn_call("__proxyAny", vec![operand.clone()], obj.span);
            let any = then.ty;
            let none = syn.expr(E::Lit(ast::Lit::Undefined));
            let els = self.expr(&none, Some(any), Want::Move);
            let init = self.mk(
                hir::ExprKind::If {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    els: Box::new(els),
                },
                any,
                obj.span,
            );
            self.hidden_local(syn.ident(&object), init, false, &mut stmts);
            operand
        };
        // The keys, once, before the loop: a proxy's `ownKeys`, else the value's own.
        let keys = syn.expr(E::Cond {
            cond: Box::new(syn.name(&is_proxy)),
            then: Box::new(call("__anyForInKeys", vec![syn.name(&object)])),
            els: Box::new(syn.method(syn.name("Object"), "keys", vec![operand])),
        });
        let inner_flag = std::mem::replace(&mut self.proxy_keys_inner, true);
        self.stmt(&decl(&keys_name, None, keys), &mut stmts);
        self.proxy_keys_inner = inner_flag;
        let keys = syn.name(&keys_name);
        let has = call(
            "__anyForInHas",
            vec![syn.name(&object), syn.expr(E::Ident(k.clone()))],
        );
        let skip = syn.binary(
            ast::BinaryOp::And,
            syn.name(&is_proxy),
            syn.expr(E::Unary {
                op: ast::UnaryOp::Not,
                expr: Box::new(has),
            }),
        );
        let test = syn.stmt(ast::StmtKind::If {
            cond: skip,
            then: syn.block(vec![syn.stmt(ast::StmtKind::Continue(None))]),
            els: None,
        });
        let mut inner = Vec::with_capacity(body.stmts.len() + 1);
        inner.push(test);
        inner.extend(body.stmts.iter().cloned());
        let lp = ast::Stmt {
            kind: ast::StmtKind::ForOf {
                kind,
                pattern: pattern.clone(),
                iter: keys,
                body: ast::Block {
                    stmts: inner,
                    span: body.span,
                },
                is_await: false,
            },
            span,
        };
        self.loop_stmt(&lp, label, &mut stmts);
        self.pop_scope();
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        Self::push(out, S::Block(block), span);
        true
    }
}
