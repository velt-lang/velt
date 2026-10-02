//! Compound assignment (`r[k] += v`, `r[k]++`) on an *open* record (`Record<string, V>`, or a
//! type-parameter key): reading a key that may be missing gives `V | null`, and JavaScript's
//! `undefined + 1` (`NaN`) has no counterpart, so these are errors with a fix-it that says what
//! a missing key starts from: `r[k] = (r[k] ?? 0) + 1`. `r[k] ??= v` stays allowed (it is
//! defined for a missing key). Closed records always have every key and keep the compound forms.
//!
//! Also the other record misuse whose fix-it names the record as written: `for...of` over a
//! record, which TypeScript code writes over `Object.keys(r)` or `Object.entries(r)`.

use velt_syntax::ast;

use super::ops::op_str;
use crate::body::FnCx;
use crate::hir::TyId;

impl FnCx<'_, '_> {
    /// Reports `op` (`"+="`, `"++"`, …) on `target`, a key of the open record type `rec` with
    /// values `v`, combining with `bop` and `value` (`None`: `++` / `--`).
    pub(super) fn open_record_compound(
        &mut self,
        rec: TyId,
        v: TyId,
        target: &ast::Expr,
        op: &str,
        bop: ast::BinaryOp,
        value: Option<&ast::Expr>,
    ) {
        let (rn, vn) = (self.cx.display(rec), self.cx.display(v));
        let place = expr_text(target).unwrap_or_else(|| "r[k]".to_string());
        let rhs = value
            .and_then(expr_text)
            .unwrap_or_else(|| if value.is_none() { "1" } else { "v" }.to_string());
        let start = if self.cx.ty.is_numeric(v) {
            Some("0")
        } else if v == self.cx.ty.str_ {
            Some("\"\"")
        } else {
            None
        };
        let fix = match start {
            Some(s) => format!(
                "say what a missing key starts from: `{place} = ({place} ?? {s}) {} {rhs}`",
                op_str(bop)
            ),
            None => format!(
                "read `{place}` into a variable, check it for `null`, then assign the new value"
            ),
        };
        self.cx.error(
            velt_common::Diagnostic::error(
                format!(
                    "cannot use `{op}` on a key of a `{rn}`: the key may be missing, so `{place}` is `{vn} | null`"
                ),
                target.span,
            )
            .with_note(fix),
        );
    }
}

impl FnCx<'_, '_> {
    /// `for (... of r)` where `r` (`iter`) is the record type `rec`: records are not iterable.
    pub(crate) fn record_not_iterable(&mut self, rec: TyId, iter: &ast::Expr) {
        let rn = self.cx.display(rec);
        let r = expr_text(iter).unwrap_or_else(|| "r".to_string());
        self.cx.error(
            velt_common::Diagnostic::error(
                format!("cannot iterate over a `{rn}` with `for...of`"),
                iter.span,
            )
            .with_note(format!(
                "iterate its keys with `for (const k of Object.keys({r}))` or its entries with `for (const [k, v] of Object.entries({r}))`"
            )),
        );
    }
}

/// The source form of a simple expression (a variable, `this`, a member or index path, a
/// literal), for fix-its; `None` for anything else.
fn expr_text(e: &ast::Expr) -> Option<String> {
    use ast::ExprKind as A;
    Some(match &e.kind {
        A::Ident(id) => id.name.clone(),
        A::This => "this".to_string(),
        A::Paren(x) => expr_text(x)?,
        A::Member {
            object,
            prop,
            optional: false,
        } => format!("{}.{}", expr_text(object)?, prop.name),
        A::Index {
            object,
            index,
            optional: false,
        } => format!("{}[{}]", expr_text(object)?, expr_text(index)?),
        A::Lit(ast::Lit::Int { value, suffix }) => {
            format!("{value}{}", suffix.as_deref().unwrap_or(""))
        }
        A::Lit(ast::Lit::Float { value, suffix }) => {
            format!("{value:?}{}", suffix.as_deref().unwrap_or(""))
        }
        A::Lit(ast::Lit::Str(s)) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        A::Lit(ast::Lit::Bool(b)) => b.to_string(),
        _ => return None,
    })
}
