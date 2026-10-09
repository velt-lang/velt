//! Indexing with a key whose type is a string literal or a union of them, as in TypeScript:
//! `const k = "a-b"; o[k]` reads the field `a-b` like `o["a-b"]`, and `o[j]` with
//! `j: "a-b" | "c"` reads the field `j` holds at run time (a test per member), its type the union
//! of the fields' types. A write through such a key needs the fields to have one type.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::member::literal_key;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, LitValue, TyId};

/// A synthesized expression (no source node of its own).
fn synth(kind: ast::ExprKind, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

/// `object["key"]`.
fn keyed(object: &ast::Expr, key: &str, span: Span) -> ast::Expr {
    let index = synth(ast::ExprKind::Lit(ast::Lit::Str(key.to_string())), span);
    synth(
        ast::ExprKind::Index {
            object: Box::new(object.clone()),
            index: Box::new(index),
            optional: false,
        },
        span,
    )
}

/// `index === "key"`.
fn is_key(index: &ast::Expr, key: &str) -> ast::Expr {
    let lit = synth(
        ast::ExprKind::Lit(ast::Lit::Str(key.to_string())),
        index.span,
    );
    synth(
        ast::ExprKind::Binary {
            op: ast::BinaryOp::Eq,
            lhs: Box::new(index.clone()),
            rhs: Box::new(lit),
        },
        index.span,
    )
}

/// Can `e` be evaluated once per member without changing what the program does (a name, `this`,
/// a field of one, a literal cast)?
fn repeatable(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This | ast::ExprKind::Lit(_) => true,
        ast::ExprKind::Paren(x) | ast::ExprKind::Cast { expr: x, .. } => repeatable(x),
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => repeatable(object),
        _ => false,
    }
}

impl FnCx<'_, '_> {
    /// The string keys index `e` can be, from a constant initialized with a string literal
    /// (`const k = "a-b"`) or a type of string literals (`j: "a-b" | "c"`, `x as "a" | "b"`);
    /// `None` for anything else (a written literal is `literal_key`'s).
    pub(crate) fn literal_keys(&mut self, e: &ast::Expr) -> Option<Vec<String>> {
        match &e.kind {
            ast::ExprKind::Paren(x) => self.literal_keys(x),
            ast::ExprKind::Ident(id) => {
                if let Some(l) = self.const_lit(id) {
                    return match l.lit {
                        ast::Lit::Str(s) => Some(vec![s]),
                        _ => None,
                    };
                }
                // Its narrowed type (`k` is `"a"` after `if (k === "a")`).
                let t = self.peek_local_ty(&id.name)?;
                self.string_literals(t)?;
                let t = self.peek_ty(e)?;
                self.string_literals(t)
            }
            ast::ExprKind::Cast { ty, .. } => {
                let t = self.resolve(ty);
                self.string_literals(t)
            }
            _ => None,
        }
    }

    /// The values of `t` when it is a string literal type or a union of them.
    fn string_literals(&mut self, t: TyId) -> Option<Vec<String>> {
        let members = self.cx.union_members(t).unwrap_or_else(|| vec![t]);
        members
            .into_iter()
            .map(|m| match self.cx.lit_value(m) {
                Some(LitValue::Str(s)) => Some(s),
                _ => None,
            })
            .collect()
    }

    /// The one key a computed index names: a written literal, or a key `literal_keys` knows
    /// (then checked as a value too, so a constant counts as used).
    pub(super) fn single_key(&mut self, index: &ast::Expr) -> Option<String> {
        if let Some(k) = literal_key(index) {
            return Some(k);
        }
        match self.literal_keys(index)?.as_slice() {
            [k] => Some(k.clone()),
            _ => None,
        }
    }

    /// `object[index]` read with an index of several literal keys, every one a field of the
    /// object (`None` otherwise, for the ordinary indexing rules): the field the key names at
    /// run time, typed as the union of the fields' types.
    pub(super) fn keyed_read(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let keys = self.union_keys(object, index)?;
        if want == Want::BorrowMut {
            self.cx.error(
                Diagnostic::error(
                    "cannot change a field through a key that names one of several fields",
                    index.span,
                )
                .with_note("narrow the key first (`if (k === \"a\")`), or assign to the field"),
            );
            return Some(self.error_expr(span));
        }
        let mut reads: Vec<hir::Expr> = keys
            .iter()
            .map(|k| self.expr(&keyed(object, k, span), None, want))
            .collect();
        let tys: Vec<TyId> = reads.iter().map(|h| h.ty).collect();
        let ty = self.cx.union_of(&tys, false, span);
        let mut out = self.coerce(reads.pop().expect("ICE: several keys"), ty);
        for (k, read) in keys.iter().zip(reads).rev() {
            let cond = self.cond(&is_key(index, k));
            let then = self.coerce(read, ty);
            let kind = H::If {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(out),
            };
            out = self.mk(kind, ty, span);
        }
        Some(out)
    }

    /// `object[index] = value` (or `op=`) with an index of several literal keys, every one a field
    /// of the object (`None` otherwise): the value is computed once and assigned to the field the
    /// key names at run time. The fields must have one type.
    pub(super) fn keyed_assign(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Index {
            object,
            index,
            optional: false,
        } = &target.kind
        else {
            return None;
        };
        let keys = self.union_keys(object, index)?;
        let t = self.peek_ty(object)?;
        let field_tys: Vec<TyId> = keys
            .iter()
            .map(|k| self.field_ty(t, k))
            .collect::<Option<_>>()?;
        if field_tys.iter().any(|t| *t != field_tys[0]) {
            let list: Vec<String> = keys
                .iter()
                .zip(&field_tys)
                .map(|(k, t)| format!("`{k}`: {}", self.cx.display(*t)))
                .collect();
            self.cx.error(
                Diagnostic::error(
                    "cannot assign through this key: the fields it may name have different types",
                    index.span,
                )
                .with_note(format!("the fields: {}", list.join(", ")))
                .with_note("narrow the key first (`if (k === \"a\")`), or assign to the field"),
            );
            self.expr(value, None, Want::Move);
            return Some(self.error_expr(span));
        }
        // `{ const v = value; if (index === k1) object[k1] op= v; else if … else object[kn] op= v; }`
        self.push_scope();
        let init = self.expr(value, Some(field_tys[0]), Want::Move);
        let init = self.coerce(init, field_tys[0]);
        let name = ast::Ident {
            name: "<assigned value>".into(),
            span: value.span,
        };
        let local = self.declare_local(&name, field_tys[0], LocalKind::Const);
        let v = synth(ast::ExprKind::Ident(name), value.span);
        let assign = |s: &mut Self, k: &str| s.assign(op, &keyed(object, k, target.span), &v, span);
        let mut chain = assign(self, keys.last().expect("ICE: several keys"));
        for k in keys.iter().rev().skip(1) {
            let cond = self.cond(&is_key(index, k));
            let then = assign(self, k);
            let ty = then.ty;
            let kind = H::If {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(chain),
            };
            chain = self.mk(kind, ty, span);
        }
        let value = chain;
        self.pop_scope();
        let ty = value.ty;
        let stmt = hir::Stmt {
            kind: hir::StmtKind::Let {
                local,
                init: Some(init),
            },
            span,
        };
        let block = hir::Block {
            stmts: vec![stmt],
            value: Some(Box::new(value)),
            span,
        };
        Some(self.mk(H::Block(block), ty, span))
    }

    /// The keys of `object[index]` when the index has several literal keys, both sides can be
    /// evaluated once per key, and every key is a field of the object.
    fn union_keys(&mut self, object: &ast::Expr, index: &ast::Expr) -> Option<Vec<String>> {
        if literal_key(index).is_some() || !repeatable(object) || !repeatable(index) {
            return None;
        }
        let keys = self.literal_keys(index).filter(|k| k.len() > 1)?;
        let t = self.peek_ty(object)?;
        if self.record_args(t).is_some() {
            return None;
        }
        keys.iter()
            .all(|k| self.field_ty(t, k).is_some())
            .then_some(keys)
    }

    /// The type of field `name` of a value of type `t` (an object type, struct or class field).
    fn field_ty(&mut self, t: TyId, name: &str) -> Option<TyId> {
        self.field_of(t, name).map(|(_, ty)| ty)
    }
}
