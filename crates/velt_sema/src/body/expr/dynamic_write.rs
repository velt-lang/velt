//! Member writes on an `any` (phase 1b of #924; hir_encodings.md "any and unknown"):
//! `x.f = v`, `x["f"] = v` and `x[k] = v` become `Intrinsic::AnySet` / `AnySetIndex`, which
//! lowering sends through the descriptor of the value held. The value is boxed; the slot
//! converts it to the member's type (a `TypeError` when it is of another type) or passes it to
//! the member's setter. On an `unknown` they are errors until it is narrowed, as in TypeScript.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::places::{is_place, set_place_mode};
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, Intrinsic, Lit, UseMode};

/// The key of a dynamic write.
enum WriteKey {
    /// `x.f` / `x["f"]`, or a `string` / `any` key.
    Str(hir::Expr),
    /// A number key.
    Num(hir::Expr),
    /// A symbol constant: the name of the member it keys and the symbol (`__anySetSym`, so a
    /// proxy's `set` trap sees the symbol).
    Sym(hir::Expr, hir::Expr),
}

impl FnCx<'_, '_> {
    /// The receiver of an assignment `obj.f = v` / `obj[k] = v`: `Err(obj)` when it is not
    /// dynamic, `Ok(Some(obj))` for an `any`, `Ok(None)` for an `unknown` (reported).
    pub(crate) fn dynamic_write_receiver(
        &mut self,
        obj: hir::Expr,
    ) -> Result<Option<hir::Expr>, hir::Expr> {
        match self.cx.dynamic(obj.ty) {
            None => Err(obj),
            Some(Dynamic::Unknown) => {
                self.unknown_use_error(obj.span);
                Ok(None)
            }
            Some(d @ (Dynamic::Object | Dynamic::Function)) => {
                self.narrow_dynamic_error(d, None, obj.span);
                Ok(None)
            }
            Some(Dynamic::Any) => {
                let mut obj = obj;
                set_place_mode(&mut obj, UseMode::Borrow);
                self.note_dyn_members();
                Ok(Some(obj))
            }
        }
    }

    /// `obj.f = v` / `obj[k] = v` (`target`) on an `any` `obj`. With `as_value`, the
    /// expression is also the value assigned (returned `true`), as in JavaScript (never the
    /// member read back, which might run a getter).
    pub(crate) fn dynamic_assign(
        &mut self,
        obj: hir::Expr,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> (hir::Expr, bool) {
        let Some(any) = self.any_ty() else {
            return (self.error_expr(span), false);
        };
        let logical = matches!(
            op,
            Some(ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish)
        );
        if op.is_some() && !logical {
            self.cx.error(
                Diagnostic::error(
                    "a compound assignment to a member of an `any` is not supported yet (#924)",
                    span,
                )
                .with_note(
                    "convert the member first and assign the result: `x.n = (x.n as number) + 1`",
                ),
            );
            self.expr(value, None, Want::Borrow);
            return (self.error_expr(span), false);
        }
        let key = match &strip(target).kind {
            ast::ExprKind::Member { prop, .. } => {
                self.note_dynamic_write(&prop.name, prop.span);
                let ty = self.cx.ty.str_;
                WriteKey::Str(self.mk(H::Lit(Lit::Str(prop.name.clone())), ty, prop.span))
            }
            ast::ExprKind::Index { index, .. } => match self.dynamic_write_key(index) {
                Some(k) => k,
                None => {
                    self.expr(value, None, Want::Borrow);
                    return (self.error_expr(span), false);
                }
            },
            _ => return (self.error_expr(span), false),
        };
        // Used as a value, the receiver and key are bound first: JavaScript evaluates them
        // before the right-hand side, which may replace the object the receiver names.
        let mut stmts = vec![];
        let (obj, key) = if as_value || logical {
            let obj = self.hoist_dynamic(obj, &mut stmts);
            let key = match key {
                WriteKey::Str(k) => WriteKey::Str(self.hoist_dynamic(k, &mut stmts)),
                WriteKey::Num(k) => WriteKey::Num(self.hoist_dynamic(k, &mut stmts)),
                WriteKey::Sym(n, k) => WriteKey::Sym(n, self.hoist_dynamic(k, &mut stmts)),
            };
            (obj, key)
        } else {
            (obj, key)
        };
        if let Some(op) = op {
            return (
                self.dynamic_logical_assign(obj, key, op, value, stmts, span),
                as_value,
            );
        }
        let v = self.expr(value, Some(any), Want::Move);
        if self.cx.ty.is_bottom(v.ty) {
            return (self.error_expr(span), false);
        }
        let v = self.coerce(v, any);
        if !as_value {
            return (self.dynamic_set(obj, key, v, span), false);
        }
        // `{ const r = x; const k = key; const t = v; r[k] = share(t); t }`.
        let tmp = self.new_local("<assigned>", any, false, span, LocalKind::Temp);
        let read = self.mk(H::Local(tmp, UseMode::Borrow), any, span);
        let share = self.intrinsic(Intrinsic::Share, vec![read], any, span);
        let set = self.dynamic_set(obj, key, share, span);
        let result = self.mk(H::Local(tmp, UseMode::Move), any, span);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: tmp,
                init: Some(v),
            },
            span,
        });
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Expr(set),
            span,
        });
        let block = hir::Block {
            stmts,
            value: Some(Box::new(result)),
            span,
        };
        (self.mk(H::Block(block), any, span), true)
    }

    /// `obj[key] ??= v`, `||=` and `&&=` on an `any` (`obj` and `key` bound in `stmts`): the
    /// member is read once, and `v` is evaluated and written only when the operator calls for
    /// it, as in JavaScript. The value is the member read or the value written.
    fn dynamic_logical_assign(
        &mut self,
        obj: hir::Expr,
        key: WriteKey,
        op: ast::BinaryOp,
        value: &ast::Expr,
        mut stmts: Vec<hir::Stmt>,
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let read = match &key {
            WriteKey::Str(k) => {
                self.intrinsic(Intrinsic::AnyGet, vec![obj.clone(), k.clone()], any, span)
            }
            WriteKey::Num(k) => {
                self.intrinsic(Intrinsic::AnyIndex, vec![obj.clone(), k.clone()], any, span)
            }
            WriteKey::Sym(n, _) if !self.uses_proxy() => {
                self.intrinsic(Intrinsic::AnyGet, vec![obj.clone(), n.clone()], any, span)
            }
            WriteKey::Sym(n, k) => {
                let args = vec![obj.clone(), n.clone(), k.clone()];
                self.prelude_hir_call("__anyGetSym", args, any, span)
            }
        };
        let cur = self.new_local("<current>", any, false, span, LocalKind::Temp);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: cur,
                init: Some(read),
            },
            span,
        });
        let b = self.cx.ty.bool_;
        let held = self.mk(H::Local(cur, UseMode::Borrow), any, span);
        let cond = match op {
            ast::BinaryOp::Nullish => {
                let null = self.dynamic_nothing(hir::Nothing::Null, any, span);
                self.intrinsic(Intrinsic::AnyLooseEq, vec![held, null], b, span)
            }
            _ => self.intrinsic(Intrinsic::AnyTruthy, vec![held], b, span),
        };
        // The write: `{ const t = v; obj[key] = share(t); t }`.
        let v = self.expr(value, Some(any), Want::Move);
        if self.cx.ty.is_bottom(v.ty) {
            return self.error_expr(span);
        }
        let v = self.coerce(v, any);
        let tmp = self.new_local("<assigned>", any, false, span, LocalKind::Temp);
        let t = self.mk(H::Local(tmp, UseMode::Borrow), any, span);
        let share = self.intrinsic(Intrinsic::Share, vec![t], any, span);
        let set = self.dynamic_set(obj, key, share, span);
        let written = hir::Block {
            stmts: vec![
                hir::Stmt {
                    kind: hir::StmtKind::Let {
                        local: tmp,
                        init: Some(v),
                    },
                    span,
                },
                hir::Stmt {
                    kind: hir::StmtKind::Expr(set),
                    span,
                },
            ],
            value: Some(Box::new(self.mk(H::Local(tmp, UseMode::Move), any, span))),
            span,
        };
        let written = self.mk(H::Block(written), any, span);
        let kept = self.mk(H::Local(cur, UseMode::Move), any, span);
        // `&&=` writes when the member is truthy; `||=` and `??=` when it is not (or nullish).
        let (then, els) = match op {
            ast::BinaryOp::And => (written, kept),
            ast::BinaryOp::Or => (kept, written),
            _ => (written, kept),
        };
        let pick = self.mk(
            H::If {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            },
            any,
            span,
        );
        let block = hir::Block {
            stmts,
            value: Some(Box::new(pick)),
            span,
        };
        self.mk(H::Block(block), any, span)
    }

    /// The write `obj[key] = v` (`v` boxed).
    fn dynamic_set(
        &mut self,
        obj: hir::Expr,
        key: WriteKey,
        v: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let unit = self.cx.ty.unit;
        match key {
            WriteKey::Str(k) => self.intrinsic(Intrinsic::AnySet, vec![obj, k, v], unit, span),
            WriteKey::Num(k) => self.intrinsic(Intrinsic::AnySetIndex, vec![obj, k, v], unit, span),
            WriteKey::Sym(n, k) => {
                self.prelude_hir_call("__anySetSym", vec![obj, n, k, v], unit, span)
            }
        }
    }

    /// `e` (a receiver or key) bound to a temporary appended to `stmts`, read back by borrow: a
    /// share of it when it is a place, else the value itself. A literal stays as it is.
    pub(super) fn hoist_dynamic(
        &mut self,
        mut e: hir::Expr,
        stmts: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        if matches!(e.kind, H::Lit(_)) {
            return e;
        }
        let (ty, span) = (e.ty, e.span);
        let copy = self.cx.is_copy(ty);
        let init = if copy {
            set_place_mode(&mut e, UseMode::Copy);
            e
        } else if is_place(&e) {
            set_place_mode(&mut e, UseMode::Borrow);
            self.intrinsic(Intrinsic::Share, vec![e], ty, span)
        } else {
            set_place_mode(&mut e, UseMode::Move);
            e
        };
        let tmp = self.new_local("<receiver>", ty, false, span, LocalKind::Temp);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: tmp,
                init: Some(init),
            },
            span,
        });
        let mode = if copy { UseMode::Copy } else { UseMode::Borrow };
        self.mk(H::Local(tmp, mode), ty, span)
    }

    /// The key of `x[index] = v` on an `any`: a string or `any` key names a member, a number
    /// key an element (`None` after reporting an error).
    fn dynamic_write_key(&mut self, index: &ast::Expr) -> Option<WriteKey> {
        // A symbol constant (`o[KEY] = v`) names the member it keys.
        if let Some(name) = self.symbol_key_of(index) {
            self.note_dynamic_write(&name, index.span);
            let str_ = self.cx.ty.str_;
            let k = self.mk(H::Lit(Lit::Str(name)), str_, index.span);
            if !self.uses_proxy() {
                return Some(WriteKey::Str(k));
            }
            let sym = self.expr(index, None, Want::Borrow);
            return Some(WriteKey::Sym(k, sym));
        }
        let k = self.expr(index, None, Want::Borrow);
        if self.cx.ty.is_bottom(k.ty) {
            return None;
        }
        let str_ = self.cx.ty.str_;
        let f64_ = self.cx.ty.f64;
        let kt = self.cx.widened(k.ty);
        if let H::Lit(Lit::Str(name)) = &k.kind {
            let name = name.clone();
            self.note_dynamic_write(&name, index.span);
        } else {
            // A key known only at run time may name any field.
            self.note_dynamic_write("", index.span);
        }
        if self.cx.dynamic(k.ty) == Some(Dynamic::Any) || kt == str_ {
            let k = if kt == str_ { self.coerce(k, str_) } else { k };
            return Some(WriteKey::Str(k));
        }
        if self.cx.ty.is_numeric(kt) {
            return Some(WriteKey::Num(self.coerce_number(k, f64_)));
        }
        if self.string_or_symbol(k.ty) {
            let any = self.any_ty()?;
            return Some(WriteKey::Str(self.coerce(k, any)));
        }
        let tn = self.cx.display(k.ty);
        self.cx.error(
            Diagnostic::error(
                format!("indexing an `any` with a `{tn}` key is not supported yet (#924)"),
                index.span,
            )
            .with_note("use a `string` or `number` key"),
        );
        None
    }

    /// A write through `any` to field `name` (empty: a key known only at run time, which may
    /// be any field) of whatever object it holds, for the copies of `crate::object_copies`.
    pub(super) fn note_dynamic_write(&mut self, name: &str, span: Span) {
        // The standard library's own writes (`Reflect.set`, a proxy's forwarding) count where
        // the program's code reaches them (`proxy.rs`).
        if self.cx.scopes[self.module].is_std {
            return;
        }
        self.cx
            .object_copies
            .writes
            .push(crate::object_copies::Write {
                ty: None,
                name: name.to_string(),
                span,
                dynamic: true,
            });
    }

    /// `x.f++`, `x.f.push(v)` and other writes through a member of an `any` that are not an
    /// assignment.
    pub(super) fn dynamic_write_error(&mut self, span: Span) {
        self.cx.error(
            Diagnostic::error(
                "changing a member of an `any` in place is not supported yet (#924)",
                span,
            )
            .with_note("assign it instead (`x.f = v`), or convert the value to its type first (`const o = x as T;`)"),
        );
    }
}

fn strip(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(inner) => strip(inner),
        _ => e,
    }
}
