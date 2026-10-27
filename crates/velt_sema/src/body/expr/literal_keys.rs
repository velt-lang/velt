//! Indexing with a key whose type is a string literal or a union of them, as in TypeScript:
//! `const k = "a-b"; o[k]` reads the field `a-b` like `o["a-b"]`, and `o[j]` with
//! `j: "a-b" | "c"` reads the field `j` holds at run time (a test per member), its type the union
//! of the fields' types. A write through such a key needs the fields to have one type.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::member::literal_key;
use super::method_value::{is_path, strip_parens};
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, LitValue, TyId, TyKind};

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

/// `object[key]`.
fn indexed(object: &ast::Expr, key: &ast::Expr, span: Span) -> ast::Expr {
    let (object, index) = (Box::new(object.clone()), Box::new(key.clone()));
    synth(
        ast::ExprKind::Index {
            object,
            index,
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
            // A field of a variable (`h.k`), with its narrowed type.
            ast::ExprKind::Member { .. } if is_path(e) => {
                let t = self.peek_ty(e)?;
                self.string_literals(t)
            }
            _ => None,
        }
    }

    /// The values of `t` when it is a string literal type or a union of them.
    pub(super) fn string_literals(&mut self, t: TyId) -> Option<Vec<String>> {
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
    pub(crate) fn single_key(&mut self, index: &ast::Expr) -> Option<String> {
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
        let redo = |s: &mut Self, key: &ast::Expr| s.expr(&indexed(object, key, span), None, want);
        self.with_union_key(object, index, span, redo, |s, keys, key| {
            if want == Want::BorrowMut {
                s.cx.error(
                    Diagnostic::error(
                        "cannot change a field through a key that names one of several fields",
                        index.span,
                    )
                    .with_note("narrow the key first (`if (k === \"a\")`), or assign to the field"),
                );
                return s.error_expr(span);
            }
            let reads: Vec<hir::Expr> = keys
                .iter()
                .map(|k| s.expr(&keyed(object, k, span), None, want))
                .collect();
            let tys: Vec<TyId> = reads.iter().map(|h| h.ty).collect();
            let ty = s.cx.union_of(&tys, false, span);
            let reads = reads.into_iter().map(|h| s.coerce(h, ty)).collect();
            s.key_chain(keys, key, reads, Some(ty), span)
        })
    }

    /// `object[index] = value` (or `op=`) with an index of several literal keys, every one a field
    /// of the object (`None` otherwise): the field the key names at run time is assigned. The
    /// fields must have one type.
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
        } = &strip_parens(target).kind
        else {
            return None;
        };
        if op.is_none() {
            if let Some(h) = self.generic_key_write(object, index, value, span) {
                return Some(h);
            }
        }
        let redo = |s: &mut Self, key: &ast::Expr| {
            s.assign(op, &indexed(object, key, target.span), value, span)
        };
        self.with_union_key(object, index, span, redo, |s, keys, key| {
            let Some(fty) = s.one_field_ty(object, index, keys) else {
                s.expr(value, None, Want::Move);
                return s.error_expr(span);
            };
            if op.is_some() {
                // `if (key === k1) object[k1] op= value; else …`: the field is read before
                // `value` runs, as in JavaScript, and only one branch runs.
                let branches = keys
                    .iter()
                    .map(|k| s.assign(op, &keyed(object, k, target.span), value, span))
                    .collect();
                return s.key_chain(keys, key, branches, None, span);
            }
            // `{ const v = value; if (key === k1) object[k1] = v; else … object[kn] = v; }`
            let init = s.expr(value, Some(fty), Want::Move);
            let init = s.coerce(init, fty);
            s.with_temp("<assigned value>", init, value.span, span, |s, v| {
                let branches = keys
                    .iter()
                    .map(|k| s.assign(None, &keyed(object, k, target.span), v, span))
                    .collect();
                s.key_chain(keys, key, branches, None, span)
            })
        })
    }

    /// `object[index]++` (or `--`, prefix or postfix) with an index of several literal keys,
    /// every one a field of the object (`None` otherwise): the field the key names at run time
    /// is updated. The fields must have one type.
    pub(crate) fn keyed_update(
        &mut self,
        op: ast::UpdateOp,
        prefix: bool,
        target: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Index {
            object,
            index,
            optional: false,
        } = &strip_parens(target).kind
        else {
            return None;
        };
        let redo = |s: &mut Self, key: &ast::Expr| {
            s.update(
                op,
                prefix,
                &indexed(object, key, target.span),
                as_value,
                span,
            )
        };
        self.with_union_key(object, index, span, redo, |s, keys, key| {
            if s.one_field_ty(object, index, keys).is_none() {
                return s.error_expr(span);
            }
            let branches = keys
                .iter()
                .map(|k| s.update(op, prefix, &keyed(object, k, target.span), as_value, span))
                .collect();
            s.key_chain(keys, key, branches, None, span)
        })
    }

    /// The one type of the fields `keys` of `object`; `None` (reported) when they differ.
    fn one_field_ty(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        keys: &[String],
    ) -> Option<TyId> {
        let t = self.peek_ty(object)?;
        let field_tys: Vec<TyId> = keys
            .iter()
            .map(|k| self.field_ty(t, k))
            .collect::<Option<_>>()?;
        if field_tys.iter().all(|t| *t == field_tys[0]) {
            return Some(field_tys[0]);
        }
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
        None
    }

    /// `if (key === k1) b1 else if … else bn`, one branch per key, typed `ty` (or as the
    /// branches are).
    fn key_chain(
        &mut self,
        keys: &[String],
        key: &ast::Expr,
        mut branches: Vec<hir::Expr>,
        ty: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let mut out = branches.pop().expect("ICE: a key chain without keys");
        for (k, then) in keys.iter().zip(branches).rev() {
            let cond = self.cond(&is_key(key, k));
            let t = ty.unwrap_or(then.ty);
            let kind = H::If {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(out),
            };
            out = self.mk(kind, t, span);
        }
        out
    }

    /// `{ const <name> = init; body(<name>) }`.
    pub(crate) fn with_temp(
        &mut self,
        name: &str,
        init: hir::Expr,
        at: Span,
        span: Span,
        body: impl FnOnce(&mut Self, &ast::Expr) -> hir::Expr,
    ) -> hir::Expr {
        self.push_scope();
        let name = ast::Ident {
            name: name.into(),
            span: at,
        };
        let local = self.declare_local(&name, init.ty, LocalKind::Const);
        let v = synth(ast::ExprKind::Ident(name), at);
        let value = body(self, &v);
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
        self.mk(H::Block(block), ty, span)
    }

    /// Runs `body` with the keys of `object[index]` and the expression holding the key, when
    /// the index has literal keys that are all fields of the object (`None` otherwise, for the
    /// ordinary indexing rules). The object must be a path (evaluated once per key); an index
    /// that is not (`o[key()]`, `o[ks[1]]`) is evaluated once, into a temporary. A key of a
    /// type parameter's value known only per instantiation (`k: K`, `K extends keyof T`) is
    /// evaluated once too, and `redo` checks the access again with the temporary as the index
    /// (`key_index.rs`).
    fn with_union_key(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        span: Span,
        redo: impl FnOnce(&mut Self, &ast::Expr) -> hir::Expr,
        body: impl FnOnce(&mut Self, &[String], &ast::Expr) -> hir::Expr,
    ) -> Option<hir::Expr> {
        if literal_key(index).is_some() || !repeatable(object) {
            return None;
        }
        let t = self.peek_ty(object)?;
        // `any` has no fixed fields: the dynamic indexing rules read any key (`xs[f()]`).
        if self.record_args(t).is_some() || self.cx.dynamic(t).is_some() {
            return None;
        }
        if repeatable(index) {
            let keys = self.literal_keys(index).filter(|k| k.len() > 1)?;
            if !keys.iter().all(|k| self.field_ty(t, k).is_some()) {
                return None;
            }
            return Some(body(self, &keys, index));
        }
        // Any other index on an object type is an error in the ordinary rules, so the index
        // can be checked here, once, whatever its type turns out to be.
        let fields = match self.cx.ty.kind(t) {
            // A std class indexed by `__index` (a typed array, a regex match) has its own rules.
            TyKind::Adt(..) => !self.has_indexer(t),
            TyKind::Dyn(..) | TyKind::Param(_) => true,
            _ => false,
        };
        if !fields {
            return None;
        }
        let h = self.expr(index, None, Want::Move);
        if self.cx.ty.has_error(h.ty) {
            return Some(self.error_expr(span));
        }
        let keys = self
            .string_literals(h.ty)
            .filter(|keys| keys.iter().all(|k| self.field_ty(t, k).is_some()));
        let at = index.span;
        let Some(keys) = keys else {
            if self.generic_keyed(t, h.ty) {
                return Some(self.with_temp("<key>", h, at, span, redo));
            }
            // As the ordinary rules report it (`o[s]` with `s: string`).
            let tn = self.cx.display(t);
            let mut d =
                Diagnostic::error(format!("cannot index a value of type `{tn}`"), object.span);
            if self.cx.class_of(t).is_some() {
                d = d.with_note("use a method such as `m.get(key)`");
            }
            self.cx.error(d);
            return Some(self.error_expr(span));
        };
        Some(self.with_temp("<key>", h, at, span, |s, key| body(s, &keys, key)))
    }

    /// The type of field `name` of a value of type `t` (an object type, struct or class field).
    fn field_ty(&mut self, t: TyId, name: &str) -> Option<TyId> {
        self.field_of(t, name).map(|(_, ty)| ty)
    }
}
