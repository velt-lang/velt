//! `Record<K, V>` (docs/internals/design/record.md): the prelude class std/prelude/record.vlt
//! with TypeScript's object syntax. `K` is `string` (an *open* record: keys may be missing) or a
//! union of string literal types / a string enum (a *closed* record: every key is present).
//!
//! - `r[k]`, `r.name`: `r.__get(k)` (`V | null`) on open records, `r.__at(k)` (`V`) on closed
//!   ones: the stored value itself, so modifying it modifies the record's entry (as in JS).
//! - `r[k] = v`: `r.__set(k, v)`; `r[k] op= v` and `++`/`--` are `r.__set(k, r[k] op v)`, so the
//!   receiver and key must be free of side effects (like setters).
//! - `delete r[k]`: `r.__delete(k)` (open records only).
//! - An object literal where a record is expected: `{ let t = new Record(); t.__set("a", ..);
//!   t.__extend(spread); t }`. A closed record's literal must have every key.
//! - A type-parameter key may stand for a closed key type: such a record is read as open (`V |
//!   null`), but cannot start empty (except from a spread) or lose a key.
//!
//! Inside the class itself (`this.entries`) none of this applies. The calls and the rule
//! against writing them by hand are in `record_call.rs`; key types in `crate::record_keys`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

pub(super) use super::record_call::{record_parts, RecordKey};
use super::setters::{side_effect_free, synth};
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, DefId, ExprKind as H, StmtKind as S, TyId, TyKind, UseMode};

/// The note on rejected constructions and deletions with a type-parameter key.
const GENERIC_KEY_NOTE: &str = "the key type may be a union of string literals or a string enum, whose records always have every key; build or change the record where its key type is known, or use `Record<string, V>`";

impl FnCx<'_, '_> {
    /// `(K, V)` if `t` is the prelude's `Record<K, V>` and the record syntax applies here (not
    /// inside the class's own methods).
    pub(crate) fn record_args(&self, t: TyId) -> Option<(TyId, TyId)> {
        let TyKind::Adt(d, args) = self.cx.ty.kind(t) else {
            return None;
        };
        if Some(*d) != self.cx.prelude_adt("Record") || self.owner == Some(*d) {
            return None;
        }
        match args.as_slice() {
            [k, v] => Some((*k, *v)),
            _ => None,
        }
    }

    /// The keys of a closed record (`None`: `K` is `string`, a type parameter or an error).
    pub(crate) fn record_keys(&mut self, k: TyId) -> Option<Vec<String>> {
        self.cx.record_key_names(k)
    }

    /// `new Record<K, V>()`: only open records start empty.
    pub(super) fn check_new_record(&mut self, rec: TyId, span: Span) -> bool {
        let Some((k, _)) = self.record_args(rec) else {
            return true;
        };
        if !self.check_record_key(k, span)
            || self.reject_generic_key(k, "cannot create an empty {R}", span)
        {
            return false;
        }
        if self.record_keys(k).is_some() {
            let kn = self.cx.display(k);
            self.cx.error(
                Diagnostic::error(
                    format!("a `Record<{kn}, ...>` cannot start empty: it always has every key"),
                    span,
                )
                .with_note("write it as an object literal with every key"),
            );
            return false;
        }
        true
    }

    /// Is `k` a valid record key type? Reports at `span` if not.
    pub(crate) fn check_record_key(&mut self, k: TyId, span: Span) -> bool {
        self.cx.check_record_key(k, span, None)
    }

    /// Reports (at `span`) that `what` (`{R}`: the record type) needs a known key type if `k`
    /// mentions a type parameter.
    fn reject_generic_key(&mut self, k: TyId, what: &str, span: Span) -> bool {
        if !self.cx.is_generic_key(k) {
            return false;
        }
        let kn = self.cx.display(k);
        self.cx.error(
            Diagnostic::error(
                format!(
                    "{}: the key type `{kn}` is a type parameter",
                    what.replace("{R}", &format!("`Record<{kn}, ...>`"))
                ),
                span,
            )
            .with_note(GENERIC_KEY_NOTE),
        );
        true
    }

    /// `r[k]` / `r.name` (`obj` is the checked record).
    pub(super) fn record_read(
        &mut self,
        obj: hir::Expr,
        key: RecordKey<'_>,
        span: Span,
    ) -> hir::Expr {
        let (k, _) = self
            .record_args(obj.ty)
            .expect("ICE: record read on a non-record");
        let closed = self.record_keys(k).is_some();
        if let (true, RecordKey::Name(id)) = (closed, &key) {
            if !self.record_has_key(k, &id.name, id.span) {
                return self.error_expr(span);
            }
        }
        let method = if closed { "__at" } else { "__get" };
        self.record_call_keyed(obj, method, key, &[], span)
    }

    fn record_has_key(&mut self, k: TyId, name: &str, span: Span) -> bool {
        let keys = self.record_keys(k).unwrap_or_default();
        if keys.iter().any(|s| s == name) {
            return true;
        }
        let kn = self.cx.display(k);
        self.cx.err(format!("`{kn}` has no key \"{name}\""), span);
        false
    }

    /// `target = value` / `target op= value` where `target` is `obj[key]` / `obj.name` of a
    /// record (`obj` checked).
    #[allow(clippy::too_many_arguments)] // target parts + assignment parts
    pub(super) fn record_assign(
        &mut self,
        obj: hir::Expr,
        object: &ast::Expr,
        key: RecordKey<'_>,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let (k, _) = self
            .record_args(obj.ty)
            .expect("ICE: record write on a non-record");
        if let RecordKey::Name(id) = &key {
            if self.record_keys(k).is_some() && !self.record_has_key(k, &id.name, id.span) {
                self.check_args_loose(std::slice::from_ref(value));
                return self.error_expr(span);
            }
        }
        if !self.require_mutable(&obj, "assign to a key of") {
            self.check_args_loose(std::slice::from_ref(value));
            return self.error_expr(span);
        }
        let value = match op {
            None => value.clone(),
            Some(op) => {
                if !side_effect_free(object) || !key.side_effect_free() {
                    self.cx.err(
                        "compound assignment to a `Record` key needs a variable receiver and a variable or literal key",
                        target.span,
                    );
                    self.check_args_loose(std::slice::from_ref(value));
                    return self.error_expr(span);
                }
                let kind = ast::ExprKind::Binary {
                    op,
                    lhs: Box::new(target.clone()),
                    rhs: Box::new(value.clone()),
                };
                synth(kind, span)
            }
        };
        self.record_call_keyed(obj, "__set", key, std::slice::from_ref(&value), span)
    }

    /// `r[k]++` / `--r.name` (not used as a value): `r.__set(k, r[k] + 1)`.
    pub(super) fn record_update(
        &mut self,
        obj: hir::Expr,
        op: ast::UpdateOp,
        target: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let opname = if op == ast::UpdateOp::Inc { "++" } else { "--" };
        if as_value {
            self.cx.err(
                format!("`{opname}` on a `Record` key cannot be used as a value"),
                span,
            );
            return self.error_expr(span);
        }
        let bop = if op == ast::UpdateOp::Inc {
            ast::BinaryOp::Add
        } else {
            ast::BinaryOp::Sub
        };
        let one = synth(
            ast::ExprKind::Lit(ast::Lit::Int {
                value: 1,
                suffix: None,
            }),
            span,
        );
        let (object, key) = record_parts(target);
        self.record_assign(obj, object, key, Some(bop), target, &one, span)
    }

    /// `delete operand`: only `delete r[k]` / `delete r.name` on an open record.
    pub(super) fn delete_expr(&mut self, operand: &ast::Expr, span: Span) -> hir::Expr {
        let (object, key) = match &operand.kind {
            ast::ExprKind::Index {
                object,
                index,
                optional: false,
            } => (object, RecordKey::Index(index)),
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } => (object, RecordKey::Name(prop)),
            ast::ExprKind::Paren(inner) => return self.delete_expr(inner, span),
            _ => {
                self.cx.err(
                    "`delete` removes a key from a `Record`: write `delete r[k]`",
                    span,
                );
                return self.error_expr(span);
            }
        };
        let obj = self.expr(object, None, Want::Borrow);
        let Some((k, _)) = self.record_args(obj.ty) else {
            if !self.cx.ty.is_bottom(obj.ty) {
                let tn = self.cx.display(obj.ty);
                self.cx.error(
                    Diagnostic::error(format!("cannot `delete` from a value of type `{tn}`"), span)
                        .with_note("`delete` only removes keys from a `Record<string, V>`"),
                );
            }
            return self.error_expr(span);
        };
        if self.record_keys(k).is_some() {
            let kn = self.cx.display(k);
            self.cx.err(
                format!("cannot `delete` from a `Record<{kn}, ...>`: it always has every key"),
                span,
            );
            return self.error_expr(span);
        }
        if self.reject_generic_key(k, "cannot `delete` from a {R}", span) {
            return self.error_expr(span);
        }
        if !self.require_mutable(&obj, "delete a key of") {
            return self.error_expr(span);
        }
        self.record_call_keyed(obj, "__delete", key, &[], span)
    }

    /// Reports why an object literal cannot build a `Record<k, ...>`, if it cannot: a bad key, an
    /// enum key, or a type-parameter key without a spread (which would bring every key).
    fn record_literal_unsupported(
        &mut self,
        k: TyId,
        props: &[ast::ObjectProp],
        span: Span,
    ) -> bool {
        if !self.check_record_key(k, span) {
            return true;
        }
        let enum_keys =
            matches!(self.cx.ty.kind(k), TyKind::Adt(..)) && self.cx.union_def(k).is_none();
        if enum_keys {
            let kn = self.cx.display(k);
            self.cx.err(
                format!("a `Record` keyed by the enum `{kn}` cannot be written as an object literal yet"),
                span,
            );
            return true;
        }
        let spread = props
            .iter()
            .any(|p| matches!(p, ast::ObjectProp::Spread(_)));
        !spread
            && self.reject_generic_key(
                k,
                "cannot build a {R} from an object literal without a spread",
                span,
            )
    }

    /// An object literal where `Record<K, V>` (`d`) is expected.
    pub(super) fn record_literal(
        &mut self,
        d: DefId,
        k: TyId,
        v: TyId,
        props: &[ast::ObjectProp],
        span: Span,
    ) -> hir::Expr {
        if self.record_literal_unsupported(k, props, span) {
            return self.error_expr(span);
        }
        let keys = self.record_keys(k);
        let ty = self.cx.ty.intern(TyKind::Adt(d, vec![k, v]));
        let new = self.mk(
            H::New {
                def: d,
                type_args: vec![k, v],
                args: vec![],
            },
            ty,
            span,
        );
        let t = self.new_local("<record>", ty, false, span, LocalKind::Temp);
        let mut stmts = vec![hir::Stmt {
            kind: S::Let {
                local: t,
                init: Some(new),
            },
            span,
        }];
        let mut seen: Vec<String> = vec![];
        let mut complete = false;
        for p in props {
            let recv = self.mk(H::Local(t, UseMode::Borrow), ty, span);
            let call = match p {
                ast::ObjectProp::Spread(e) => {
                    complete = true;
                    self.record_call(recv, "__extend", std::slice::from_ref(e), e.span)
                }
                ast::ObjectProp::KeyValue(name, _) | ast::ObjectProp::Shorthand(name) => {
                    if keys.is_some() && !self.record_has_key(k, &name.name, name.span) {
                        continue;
                    }
                    if seen.contains(&name.name) {
                        self.cx.err(
                            format!("duplicate key `{}` in object literal", name.name),
                            name.span,
                        );
                        continue;
                    }
                    seen.push(name.name.clone());
                    let value = match p {
                        ast::ObjectProp::KeyValue(_, value) => value.clone(),
                        _ => synth(ast::ExprKind::Ident(name.clone()), name.span),
                    };
                    let key = RecordKey::Name(name);
                    self.record_call_keyed(recv, "__set", key, std::slice::from_ref(&value), span)
                }
            };
            stmts.push(hir::Stmt {
                kind: S::Expr(call),
                span,
            });
        }
        if let (Some(keys), false) = (&keys, complete) {
            let missing: Vec<&String> = keys.iter().filter(|k| !seen.contains(k)).collect();
            if !missing.is_empty() {
                let list: Vec<String> = missing.iter().map(|k| format!("\"{k}\"")).collect();
                let kn = self.cx.display(k);
                self.cx.err(
                    format!(
                        "missing key {} in a `Record<{kn}, ...>` literal",
                        list.join(", ")
                    ),
                    span,
                );
            }
        }
        let value = self.mk(H::Local(t, UseMode::Move), ty, span);
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }
}
