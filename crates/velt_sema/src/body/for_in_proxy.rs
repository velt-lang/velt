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
use crate::hir::{self, ExprKind as H, Intrinsic, StmtKind as S, UseMode};

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
        let h = self.expr(obj, None, Want::Borrow);
        let init = self.share_if_place(h);
        let oty = init.ty;
        let o = self.hidden_local(syn.ident(&object), init, false, &mut stmts);
        let test = if any {
            let e = syn.expr(E::InstanceOf {
                expr: Box::new(syn.name(&object)),
                ty: syn.named_type("Proxy"),
            });
            self.expr(&e, None, Want::Move)
        } else {
            let read = self.mk(H::Local(o, UseMode::Borrow), oty, obj.span);
            self.intrinsic(Intrinsic::IsTypedProxy, vec![read], bool_, obj.span)
        };
        self.hidden_local(syn.ident(&is_proxy), test, false, &mut stmts);
        let call = |f: &str, args: Vec<ast::Expr>| syn.call(syn.name(f), args);
        let keys = syn.expr(E::Cond {
            cond: Box::new(syn.name(&is_proxy)),
            then: Box::new(call("__anyForInKeys", vec![syn.name(&object)])),
            els: Box::new(syn.method(syn.name("Object"), "keys", vec![syn.name(&object)])),
        });
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
