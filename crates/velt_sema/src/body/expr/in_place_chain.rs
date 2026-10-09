//! Chaining on the in-place array methods: `xs.sort().join(",")`, `xs.reverse().length`,
//! `s.split(",").sort().map(f)`. In JS `sort`, `reverse` and `fill` return the array itself; in
//! Velt they return nothing, since returning the array would share it, which makes every array
//! of its type reference counted (std/prelude/array.vlt). Used as the receiver of a member
//! access, though, the result is only borrowed, so the chain reads the array they changed:
//! - on a place (`xs`, `this.items`, `rows[i]`) the place is read again after the call;
//! - on a temporary (`s.split(",")`) the temporary is kept in a local of a block expression,
//!   changed there and then borrowed, and dropped at the end of the block.
//!
//! Other uses of the result (`return xs.sort()`, `const ys = xs.sort()`) stay errors whose note
//! names the fix (`toSorted`, or call it on its own line).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::set_place_mode;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, StmtKind as S, UseMode};

/// The receiver of a member access, with the statements that must run before it.
pub(super) struct Receiver {
    pub before: Vec<hir::Stmt>,
    pub recv: hir::Expr,
}

impl FnCx<'_, '_> {
    /// `recv`, checked from `object`, as the receiver of a member access: when `object` is a
    /// call of `sort`, `reverse` or `fill` on an array, the call runs first and the receiver is
    /// the array it changed.
    pub(super) fn in_place_receiver(&mut self, object: &ast::Expr, recv: hir::Expr) -> Receiver {
        // `a.sort().reverse()`: the inner chain's statements run first, then this call.
        let (mut before, mut recv) = match recv.kind {
            H::Block(hir::Block {
                stmts,
                value: Some(value),
                ..
            }) if matches!(value.kind, H::Call { .. }) => (stmts, *value),
            kind => (vec![], hir::Expr { kind, ..recv }),
        };
        let unit = self.cx.ty.unit;
        let array = match &mut recv.kind {
            H::Call { args, .. } if recv.ty == unit && in_place_call(object).is_some() => args
                .first_mut()
                .filter(|a| self.cx.ty.array_elem(a.ty).is_some()),
            _ => None,
        };
        let Some(array) = array else {
            return Receiver {
                recv: self.after_receiver(before, recv),
                before: vec![],
            };
        };
        let span = recv.span;
        if is_pure_place(array) {
            let again = array.clone();
            before.push(stmt(S::Expr(recv), span));
            return Receiver {
                before,
                recv: again,
            };
        }
        // A temporary: keep it in a local, change it there, then borrow it.
        let ty = array.ty;
        let tmp = self.new_local("<changed array>", ty, true, span, LocalKind::Temp);
        let changed = self.mk(H::Local(tmp, UseMode::BorrowMut), ty, span);
        let mut temporary = std::mem::replace(array, changed);
        set_place_mode(&mut temporary, UseMode::Move);
        let keep = S::Let {
            local: tmp,
            init: Some(temporary),
        };
        before.extend([stmt(keep, span), stmt(S::Expr(recv), span)]);
        Receiver {
            before,
            recv: self.mk(H::Local(tmp, UseMode::Borrow), ty, span),
        }
    }

    /// `access`, run after `before` (a block expression when there are statements).
    pub(super) fn after_receiver(
        &mut self,
        before: Vec<hir::Stmt>,
        access: hir::Expr,
    ) -> hir::Expr {
        if before.is_empty() {
            return access;
        }
        let (ty, span) = (access.ty, access.span);
        let block = hir::Block {
            stmts: before,
            value: Some(Box::new(access)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }
}

impl FnCx<'_, '_> {
    /// The note for code that uses the result of `xs.sort()`, `xs.reverse()` or `xs.fill(v)`
    /// on an array (`h`, checked from `e`) as a value other than a receiver, as in TypeScript.
    pub(crate) fn in_place_note(&self, e: &ast::Expr, h: &hir::Expr) -> Option<String> {
        let call = match &h.kind {
            H::Block(hir::Block {
                value: Some(value), ..
            }) => value,
            _ => h,
        };
        let H::Call { args, .. } = &call.kind else {
            return None;
        };
        let on_array = args
            .first()
            .is_some_and(|a| self.cx.ty.array_elem(a.ty).is_some());
        if h.ty != self.cx.ty.unit || !on_array {
            return None;
        }
        let (object, m) = in_place_call(e)?;
        let call = if m == "fill" {
            "fill(v)"
        } else {
            &format!("{m}()")
        };
        let what = format!(
            "`{m}` changes the array in place and returns nothing in Velt (TypeScript returns the \
             same array; sharing it would make every array of its type reference counted)"
        );
        let then_use = if is_ast_place(object) {
            let xs = crate::body::switch::cases::source_text(object);
            format!("call `{xs}.{call};` on its own line, then use `{xs}`")
        } else {
            format!("store the array in a variable first (`const a = …; a.{call};`), then use `a`")
        };
        let copy = match m {
            "sort" => "; for a sorted copy that leaves the array unchanged, call `toSorted`",
            "reverse" => "; for a reversed copy that leaves the array unchanged, call `toReversed`",
            _ => "",
        };
        Some(format!(
            "{what}: {then_use}{copy}; a chained call such as `.{call}.join()` works as in \
             TypeScript"
        ))
    }

    /// `e` (checked as `h`) where a value of type `exp` is expected: reports the result of an
    /// in-place array method, which is `void`, with the fix; false for anything else.
    pub(crate) fn in_place_misuse(&mut self, e: &ast::Expr, h: &hir::Expr, exp: hir::TyId) -> bool {
        if exp == self.cx.ty.unit || self.cx.ty.is_bottom(exp) {
            return false;
        }
        let Some(note) = self.in_place_note(e, h) else {
            return false;
        };
        let expected = self.cx.display(exp);
        self.cx.error(
            velt_common::Diagnostic::error(format!("expected `{expected}`, found `void`"), e.span)
                .with_note(note),
        );
        true
    }
}

/// The receiver and method name of `x.sort(…)`, `x.reverse()` or `x.fill(…)` (possibly
/// parenthesized).
fn in_place_call(e: &ast::Expr) -> Option<(&ast::Expr, &str)> {
    match &e.kind {
        ast::ExprKind::Paren(inner) => in_place_call(inner),
        ast::ExprKind::Call { callee, .. } => match &callee.kind {
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } if matches!(prop.name.as_str(), "sort" | "reverse" | "fill") => {
                Some((object, prop.name.as_str()))
            }
            _ => None,
        },
        _ => None,
    }
}

/// A variable or a field path of one (`xs`, `this.items`), as opposed to a temporary.
fn is_ast_place(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_ast_place(object),
        ast::ExprKind::Paren(inner) => is_ast_place(inner),
        _ => false,
    }
}

/// A place that reads the same value again without running anything: a local and field or
/// index projections of one (with a local or literal index).
fn is_pure_place(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Local(..) => true,
        H::Field { base, .. } | H::UnwrapSome(base, _) | H::Downcast(base) => is_pure_place(base),
        H::UnwrapVariant { expr, .. } => is_pure_place(expr),
        H::Index { base, index, .. } => {
            is_pure_place(base) && matches!(index.kind, H::Local(..) | H::Lit(_))
        }
        _ => false,
    }
}

fn stmt(kind: S, span: Span) -> hir::Stmt {
    hir::Stmt { kind, span }
}
