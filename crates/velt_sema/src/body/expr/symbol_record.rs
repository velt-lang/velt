//! `(v as Record<symbol, unknown>)[KEY]` (https://veltlang.com/docs/reference/types/ "Record"): a look at a value's
//! symbol-keyed member, as sigx's brand checks do (`isCell`). When `v` is not a record, the read
//! goes through `any`, as `(v as any)[KEY]` does, and gives the record's value type (`unknown`,
//! or `any`). A key that is not a symbol (`["a"]`, a `string` or `symbol | string` variable) is
//! TypeScript's TS7053. Any other `Record<symbol, V>` is a `Record<string, V>`
//! (`Ctx::number_key_as_string`), whose symbol keys are a table of their own: so is the cast of
//! a record, read here or kept, written and given properties (sigx's request bags).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::callback::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, TyKind};

/// How a `Record<symbol, V>` cast is made (`FnCx::symbol_record_cast`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SymbolCast {
    Plain,
    Checked,
    Refused,
}

impl FnCx<'_, '_> {
    /// `(v as Record<symbol, V>)[index]` read (`object` is the parenthesized cast), `V` being
    /// `unknown` or `any`: `None` when `object` is not such a cast.
    pub(super) fn symbol_record_read(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let (expr, ty) = symbol_record_cast(object)?;
        let value = symbol_record_value(ty)?;
        // A record's own symbol table: an ordinary record cast and read.
        if self
            .trial_type(expr)
            .is_some_and(|t| self.record_args(t).is_some())
        {
            return None;
        }
        // The key's type: a symbol, or TypeScript's TS7053. A name, literal or member read is
        // checked on its own first (evaluating it has no effect the read below repeats).
        let simple = matches!(
            index.kind,
            ast::ExprKind::Ident(_)
                | ast::ExprKind::Lit(_)
                | ast::ExprKind::Template { .. }
                | ast::ExprKind::Member { .. }
        );
        if !simple {
            // A call (`[mk()]`) would be evaluated twice to know its type first.
            self.cx.error(
                Diagnostic::error(
                    "a computed key of a `Record<symbol, ...>` cast is not supported yet",
                    index.span,
                )
                .with_note("hold the key in a variable first: `const k = mk(); (v as Record<symbol, unknown>)[k]`"),
            );
            self.expr(expr, None, Want::Borrow);
            return Some(self.error_expr(span));
        }
        let found = {
            let k = self.expr(index, None, Want::Borrow);
            let ok = self.is_symbol_key(k.ty)
                || self.cx.ty.is_bottom(k.ty)
                || matches!(self.cx.dynamic(k.ty), Some(crate::any::Dynamic::Any));
            (!ok).then(|| self.cx.display(k.ty))
        };
        if let Some(found) = found {
            self.cx.error(
                Diagnostic::error(
                    format!("a `Record<symbol, ...>` is indexed by symbols, not by `{found}` (TypeScript's TS7053)"),
                    index.span,
                )
                .with_note("Velt reads a symbol-keyed member through it: `(v as Record<symbol, unknown>)[KEY]`, with `KEY` a symbol"),
            );
            self.expr(expr, None, Want::Borrow);
            return Some(self.error_expr(span));
        }
        let any = ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ast::Ident {
                    name: "any".into(),
                    span: ty.span,
                }],
                args: vec![],
            },
            span: ty.span,
        };
        let cast = synth(
            ast::ExprKind::Cast {
                expr: Box::new(expr.clone()),
                ty: any,
            },
            object.span,
        );
        let read = synth(
            ast::ExprKind::Index {
                object: Box::new(synth(ast::ExprKind::Paren(Box::new(cast)), object.span)),
                index: Box::new(index.clone()),
                optional: false,
                more: Vec::new(),
            },
            span,
        );
        let h = self.expr(&read, None, Want::Move);
        let v = self.resolve(value);
        Some(self.coerce(h, v))
    }

    /// How `v as Record<symbol, V>` (`target`, resolved; `None` for any other cast) is made. Such
    /// a cast is the record's own symbol table, which only a record of `V` has:
    /// - of a record of `V`: the record itself;
    /// - of a dynamic value (`object`, `unknown`, `any`) with `V` `unknown` or `any`: the record
    ///   it holds, checked where it is made (`SymbolCast::Checked`): another object would
    ///   convert to a new record, a view whose symbol keys the object never sees;
    /// - of anything else (an object or class value, an interface, a record of another type):
    ///   an error, reported here. It would be such a copy.
    pub(super) fn symbol_record_cast(
        &mut self,
        expr: &ast::Expr,
        ty: &ast::TypeExpr,
        target: hir::TyId,
        span: Span,
    ) -> Option<SymbolCast> {
        let ast::TypeExprKind::Named { path, args } = &ty.kind else {
            return None;
        };
        let symbol_key = matches!(&args[..], [ast::TypeExpr { kind: ast::TypeExprKind::Named { path: k, args: ka }, .. }, _]
            if ka.is_empty() && k.len() == 1 && k[0].name == "symbol");
        if path.len() != 1 || path[0].name != "Record" || !symbol_key {
            return None;
        }
        let (_, v) = self.record_args(target)?;
        let from = self.trial_type(expr)?;
        if self.record_args(from).is_some_and(|(_, fv)| fv == v) {
            return Some(SymbolCast::Plain);
        }
        let dynamic_value = symbol_record_value(ty).is_some();
        if dynamic_value && self.cx.dynamic(from).is_some() {
            return Some(SymbolCast::Checked);
        }
        let (shown, found) = (self.cx.display(v), self.cx.display(from));
        let d = Diagnostic::error(
            format!("a `Record<symbol, {shown}>` cast of a `{found}` is not supported yet"),
            span,
        );
        self.cx.error(match dynamic_value {
            true => d.with_note("Velt keeps symbol keys in a record's own table: the cast would make a new record, whose keys the value never sees. Read a member right away (`(v as Record<symbol, unknown>)[KEY]`), or keep the symbol keys on a `Record<string, unknown>`"),
            false => d.with_note("Velt would convert each of its string-keyed entries too: cast to `Record<symbol, unknown>` and the member you read (`(r[KEY] as T)`)"),
        });
        Some(SymbolCast::Refused)
    }

    /// Is `t` a symbol, or a union of symbols (`typeof S | typeof T`)?
    fn is_symbol_key(&mut self, t: hir::TyId) -> bool {
        if matches!(self.cx.ty.kind(t), TyKind::Symbol) {
            return true;
        }
        match self.cx.union_members(t) {
            Some(ms) => ms
                .iter()
                .all(|m| matches!(self.cx.ty.kind(*m), TyKind::Symbol)),
            None => false,
        }
    }

    /// `(v as Record<symbol, V>)[Symbol.iterator]` (a well-known symbol, which the parser gives
    /// as a member): not supported yet, as reading one through `any` is not (#924).
    pub(super) fn symbol_record_member(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        span: Span,
    ) -> Option<hir::Expr> {
        if !prop.name.starts_with("[Symbol.") {
            return None;
        }
        let (expr, ty) = symbol_record_cast(object)?;
        symbol_record_value(ty)?;
        self.cx.error(Diagnostic::error(
            format!(
                "reading `{}` through a `Record<symbol, ...>` cast is not supported yet (#924)",
                prop.name.trim_start_matches('[').trim_end_matches(']')
            ),
            prop.span,
        ));
        self.expr(expr, None, Want::Borrow);
        Some(self.error_expr(span))
    }
}

/// The value and the type of `(v as T)` (in parentheses or not) when `T` is a
/// `Record<symbol, V>` with `V` `unknown` or `any`.
fn symbol_record_cast(e: &ast::Expr) -> Option<(&ast::Expr, &ast::TypeExpr)> {
    match &e.kind {
        ast::ExprKind::Paren(x) => symbol_record_cast(x),
        ast::ExprKind::Cast { expr, ty } if symbol_record_value(ty).is_some() => Some((expr, ty)),
        _ => None,
    }
}

/// `V` when `t` is `Record<symbol, V>` with `V` `unknown` or `any`.
fn symbol_record_value(t: &ast::TypeExpr) -> Option<&ast::TypeExpr> {
    let named = |t: &ast::TypeExpr, names: &[&str]| {
        matches!(&t.kind, ast::TypeExprKind::Named { path, args }
            if args.is_empty() && path.len() == 1 && names.contains(&path[0].name.as_str()))
    };
    let ast::TypeExprKind::Named { path, args } = &t.kind else {
        return None;
    };
    let record = path.len() == 1 && path[0].name == "Record";
    match &args[..] {
        [k, v] if record && named(k, &["symbol"]) && named(v, &["unknown", "any"]) => Some(v),
        _ => None,
    }
}
