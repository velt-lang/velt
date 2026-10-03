//! Assignment, compound assignment, `++`/`--` and the ternary operator. Targets that name a
//! setter are handed to [`setters`](super::setters).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::ops::{hir_binop, untyped};
use crate::body::narrow::is_null;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, BinOp, ExprKind as H, TyId, UseMode};

/// What an assignment writes to.
enum AssignTarget {
    /// A writable place (`BorrowMut` modes).
    Place(hir::Expr),
    /// `obj.name` where `name` is a setter: the checked receiver.
    Setter(hir::Expr),
    /// `r[k]` / `r.name` of a `Record`: the checked record.
    Record(hir::Expr),
}

impl FnCx<'_, '_> {
    /// Resolve an assignment target to a writable place or a setter. Reports errors.
    fn assign_target(&mut self, target: &ast::Expr, span: Span) -> Option<AssignTarget> {
        if self.reject_env_assign(target) {
            return None;
        }
        let place = match &target.kind {
            ast::ExprKind::Paren(inner) => return self.assign_target(inner, span),
            ast::ExprKind::Ident(id) => self.assign_local(id, span),
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } => {
                if self.static_field_target(object, prop) {
                    return None;
                }
                let obj = self.expr(object, None, Want::Borrow);
                if self.record_args(obj.ty).is_some() {
                    return Some(AssignTarget::Record(obj));
                }
                if self.has_setter(obj.ty, &prop.name) {
                    return Some(AssignTarget::Setter(obj));
                }
                if self.reject_getter_assign(obj.ty, prop) {
                    return None;
                }
                if prop.name == "length" && self.cx.ty.array_elem(obj.ty).is_some() {
                    self.reject_length_assign(object, target.span);
                    return None;
                }
                let place = self.field_access(obj, prop, Want::BorrowMut, target.span)?;
                self.check_readonly(&place, prop);
                self.require_mutable(&place, "assign to a field of");
                Some(place)
            }
            ast::ExprKind::Index {
                object,
                index,
                optional: false,
            } => {
                let obj = self.expr(object, None, Want::Borrow);
                if self.record_args(obj.ty).is_some() {
                    return Some(AssignTarget::Record(obj));
                }
                let place = self.index_of(obj, index, Want::BorrowMut, target.span);
                if self.cx.ty.is_bottom(place.ty) {
                    return None;
                }
                self.require_mutable(&place, "assign to an element of");
                Some(place)
            }
            ast::ExprKind::This => {
                self.cx.err("cannot assign to `this`", target.span);
                None
            }
            _ => {
                self.cx
                    .err("invalid left-hand side of assignment", target.span);
                None
            }
        };
        place.map(AssignTarget::Place)
    }

    /// `xs.length = n`: arrays have no empty slots to grow into, so shortening is a method.
    fn reject_length_assign(&mut self, object: &ast::Expr, span: Span) {
        let xs = crate::body::switch::cases::source_text(object);
        self.cx.error(
            Diagnostic::error("the length of an array cannot be assigned", span)
                .with_note(format!("to shorten it, write `{xs}.truncate(n)`")),
        );
    }

    fn assign_local(&mut self, id: &ast::Ident, span: Span) -> Option<hir::Expr> {
        let Some(l) = self.lookup_local(&id.name, id.span) else {
            if self.lookup_item(&id.name, id.span).is_some() {
                self.cx.err(
                    format!("cannot assign to `{}`: it is not a variable", id.name),
                    span,
                );
            } else {
                self.unknown_name(id);
            }
            return None;
        };
        self.rec_local(id.span, l);
        let name = &id.name;
        let mutable = self.f.locals[l.0 as usize].mutable;
        let problem = match self.local_kind(l) {
            LocalKind::Const => Some((format!("cannot assign twice to const `{name}`"), None)),
            LocalKind::Using => Some((
                format!("cannot assign to `{name}`, a `using` declaration"),
                Some("it is disposed at the end of its block; declare another variable"),
            )),
            LocalKind::Elem if !mutable => Some((
                format!("cannot assign to `{name}`, which borrows an array element"),
                Some("index the array to replace an element: `xs[i] = v`"),
            )),
            LocalKind::Bind if !mutable => {
                Some((format!("cannot assign to pattern binding `{name}`"), None))
            }
            _ => None,
        };
        if let Some((msg, note)) = problem {
            let mut d = Diagnostic::error(msg, span);
            if let Some(n) = note {
                d = d.with_note(n);
            }
            self.cx.error(d);
        }
        self.mark_mutated(l, id.span);
        let ty = self.local_ty(l);
        Some(self.mk(H::Local(l, UseMode::BorrowMut), ty, id.span))
    }

    /// `Type.NAME = v` on a `static readonly` field (reported; statics are constants).
    fn static_field_target(&mut self, object: &ast::Expr, prop: &ast::Ident) -> bool {
        let ast::ExprKind::Ident(id) = &object.kind else {
            return false;
        };
        if self.is_local_name(&id.name) {
            return false;
        }
        let Some(crate::ctx::Item::Def(d)) = self.lookup_item(&id.name, id.span) else {
            return false;
        };
        if !self
            .cx
            .adt(d)
            .is_some_and(|a| a.statics.contains_key(&prop.name))
        {
            return false;
        }
        self.cx.err(
            format!(
                "cannot assign to `{}.{}`: static fields are readonly",
                id.name, prop.name
            ),
            prop.span,
        );
        true
    }

    fn check_readonly(&mut self, place: &hir::Expr, prop: &ast::Ident) {
        let H::Field { base, index, .. } = &place.kind else {
            return;
        };
        let Some((d, _)) = self.adt_of(base.ty) else {
            return;
        };
        let readonly = self
            .cx
            .adt(d)
            .and_then(|a| a.fields.get(*index as usize))
            .is_some_and(|f| f.readonly);
        let in_ctor = self.f.kind == crate::defs::FnKind::Ctor
            && matches!(base.kind, H::Local(l, _) if self.local_kind(l) == LocalKind::This);
        if readonly && !in_ctor {
            self.cx.err(
                format!("cannot assign to `{}`: it is a readonly field", prop.name),
                prop.span,
            );
        }
    }

    /// A read of a place that was built for writing (compound assignment / `++` value).
    fn place_read(&mut self, place: &hir::Expr, want: Want) -> hir::Expr {
        let mut r = place.clone();
        let m = self.use_mode(r.ty, want);
        set_place_mode(&mut r, m);
        r
    }

    /// An assignment whose value is used (`(line = next()) != null`, `y = (x = 5) + 1`): the
    /// assignment, then the target read again. Where no value is wanted (`void` expected, e.g.
    /// an arrow body returning nothing) it stays a statement. Targets that are not plain
    /// variables or fields of them would be evaluated twice and are not supported as values.
    pub(crate) fn assign_value(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let a = self.assign(op, target, value, span);
        if exp == Some(self.cx.ty.unit) || self.cx.ty.is_bottom(a.ty) {
            return a;
        }
        if !is_plain_path(target) {
            self.cx.err(
                "an assignment to an element or a computed place cannot be used as a value",
                span,
            );
            return self.error_expr(span);
        }
        let v = self.expr(target, None, Want::Borrow);
        let ty = v.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(a),
                span,
            }],
            value: Some(Box::new(v)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    pub(crate) fn assign(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        if let Some(op @ (ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish)) = op {
            // `a ??= b` is `a = a ?? b` (likewise `&&=`, `||=`), and the assignment narrows `a`
            // as usual. That reads the target twice, which is only right when reading it has no
            // effect (JS evaluates `xs[f()]` once).
            if !is_pure_place(target) {
                self.cx.error(
                    Diagnostic::error(
                        "`??=`, `||=` and `&&=` are not supported yet on a target that calls a function",
                        target.span,
                    )
                    .with_note("store the index or object in a variable first: `const k = f(); m[k] ??= v;`"),
                );
                self.expr(value, None, Want::Borrow);
                return self.error_expr(span);
            }
            let rhs = ast::Expr {
                id: ast::NodeId(u32::MAX),
                kind: ast::ExprKind::Binary {
                    op,
                    lhs: Box::new(target.clone()),
                    rhs: Box::new(value.clone()),
                },
                span: value.span,
            };
            return self.assign(None, target, &rhs, span);
        }
        let unit = self.cx.ty.unit;
        let place = match self.assign_target(target, span) {
            Some(AssignTarget::Place(place)) => place,
            Some(AssignTarget::Setter(obj)) => {
                return self.setter_assign(obj, op, target, value, span)
            }
            Some(AssignTarget::Record(obj)) => {
                let (object, key) = super::record::record_parts(target);
                return self.record_assign(obj, object, key, op, target, value, span);
            }
            None => {
                self.expr(value, None, Want::Borrow);
                return self.error_expr(span);
            }
        };
        let lty = place.ty;
        let Some(op) = op else {
            let v = self.expr_coerce(value, lty, Want::Move);
            self.unnarrow_fields(target);
            if let H::Local(l, _) = place.kind {
                self.check_capture_assign(l, target.span);
                self.unnarrow(l);
            }
            // Assigning a non-null value narrows the target like a check would (TS).
            if matches!(v.kind, H::WrapSome(_)) {
                let token = match place.kind {
                    H::Local(l, _) => Some(l),
                    _ => self.field_token(target),
                };
                if let Some(t) = token {
                    self.narrow(&crate::body::narrow::Fact::NonNull(t));
                }
            }
            let kind = H::Assign {
                place: Box::new(place),
                value: Box::new(v),
            };
            return self.mk(kind, unit, span);
        };
        if lty == self.cx.ty.str_ && op == ast::BinaryOp::Add {
            let v = self.expr_coerce(value, lty, Want::Borrow);
            let cur = self.place_read(&place, Want::Borrow);
            let cat = self.concat(cur, v, span);
            let kind = H::Assign {
                place: Box::new(place),
                value: Box::new(cat),
            };
            return self.mk(kind, unit, span);
        }
        let v = self.expr(value, Some(lty), Want::Borrow);
        let v = if self.cx.ty.is_float(lty) && self.is_inferred_int(&v) {
            self.int_to_float(v, lty)
        } else {
            v
        };
        if self.check_operands(op, lty, &v, span).is_none() {
            return self.error_expr(span);
        }
        if op == ast::BinaryOp::Div {
            self.check_int_div_assign(&place, &v, span);
        }
        let bop = hir_binop(op).expect("ICE: logical compound op");
        let kind = H::CompoundAssign {
            op: bop,
            place: Box::new(place),
            value: Box::new(v),
        };
        self.mk(kind, unit, span)
    }

    /// `++x` / `x--`. `as_value`: the result is used (needs the Block encoding).
    pub(crate) fn update(
        &mut self,
        op: ast::UpdateOp,
        prefix: bool,
        target: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let place = match self.assign_target(target, span) {
            Some(AssignTarget::Place(place)) => place,
            Some(AssignTarget::Setter(obj)) => {
                return self.setter_update(obj, op, target, as_value, span)
            }
            Some(AssignTarget::Record(obj)) => {
                return self.record_update(obj, op, target, as_value, span)
            }
            None => return self.error_expr(span),
        };
        let lty = place.ty;
        let opname = if op == ast::UpdateOp::Inc { "++" } else { "--" };
        if !self.cx.ty.is_numeric(lty) {
            if !self.cx.ty.is_bottom(lty) {
                let tn = self.cx.display(lty);
                self.cx
                    .err(format!("cannot apply `{opname}` to type `{tn}`"), span);
            }
            return self.error_expr(span);
        }
        let one = if self.cx.ty.is_float(lty) {
            hir::Lit::Float(1.0)
        } else {
            hir::Lit::Int(1)
        };
        let one = self.mk(H::Lit(one), lty, span);
        let bop = if op == ast::UpdateOp::Inc {
            BinOp::Add
        } else {
            BinOp::Sub
        };
        let unit = self.cx.ty.unit;
        let read = self.place_read(&place, Want::Borrow);
        let ca = self.mk(
            H::CompoundAssign {
                op: bop,
                place: Box::new(place),
                value: Box::new(one),
            },
            unit,
            span,
        );
        if !as_value {
            return ca;
        }
        self.update_value(ca, read, prefix, span)
    }

    /// `++x` as a value: `{ x += 1; x }`; `x++`: `{ let tmp = x; x += 1; tmp }`.
    fn update_value(
        &mut self,
        ca: hir::Expr,
        read: hir::Expr,
        prefix: bool,
        span: Span,
    ) -> hir::Expr {
        let lty = read.ty;
        let ca_stmt = hir::Stmt {
            kind: hir::StmtKind::Expr(ca),
            span,
        };
        let (stmts, value) = if prefix {
            (vec![ca_stmt], read)
        } else {
            let tmp = self.new_local("<postfix>", lty, false, span, LocalKind::Temp);
            let let_tmp = hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(read),
                },
                span,
            };
            (
                vec![let_tmp, ca_stmt],
                self.mk(H::Local(tmp, UseMode::Copy), lty, span),
            )
        };
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), lty, span)
    }

    pub(crate) fn ternary(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Expr,
        els: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let (when_true, when_false) = self.narrowing(cond);
        let c = self.cond(cond);
        let branch = |s: &mut Self,
                      e: &ast::Expr,
                      narrow: &[crate::body::narrow::Fact],
                      hint: Option<TyId>| {
            s.push_scope();
            narrow.iter().for_each(|f| s.narrow(f));
            let h = s.expr(e, hint, Want::Move);
            s.pop_scope();
            h
        };
        let late = |e: &ast::Expr| untyped(e) || is_null(e);
        let before = self.narrow_state();
        let (t, f) = if late(then) && !late(els) {
            let f = branch(self, els, &when_false, exp);
            let after = self.narrow_state();
            self.restore_narrowing(&before);
            let h = self.second_hint(exp, &f, then);
            let t = branch(self, then, &when_true, h);
            self.meet_narrowing(&after);
            (t, f)
        } else {
            let t = branch(self, then, &when_true, exp);
            let after = self.narrow_state();
            self.restore_narrowing(&before);
            let h = self.second_hint(exp, &t, els);
            let f = branch(self, els, &when_false, h);
            self.meet_narrowing(&after);
            (t, f)
        };
        let (t, f, ty) = self.unify_branches(t, f, exp);
        let kind = H::If {
            cond: Box::new(c),
            then: Box::new(t),
            els: Box::new(f),
        };
        self.mk(kind, ty, span)
    }

    /// Expected type of the second-checked branch: the context's, else the first branch's
    /// (made nullable when the second branch is `null`).
    fn second_hint(
        &mut self,
        exp: Option<TyId>,
        first: &hir::Expr,
        second: &ast::Expr,
    ) -> Option<TyId> {
        if exp.is_some_and(|e| !self.cx.ty.has_error(e)) {
            return exp;
        }
        if self.cx.ty.is_bottom(first.ty) {
            return exp;
        }
        if is_null(second) && self.cx.ty.opt_payload(first.ty).is_none() {
            return Some(self.cx.ty.option(first.ty));
        }
        Some(first.ty)
    }

    /// Common type of two branches (coercing one to the other, or both to `exp`).
    pub(crate) fn unify_branches(
        &mut self,
        t: hir::Expr,
        f: hir::Expr,
        exp: Option<TyId>,
    ) -> (hir::Expr, hir::Expr, TyId) {
        let (never, error) = (self.cx.ty.never, self.cx.ty.error);
        if t.ty == f.ty {
            let ty = t.ty;
            return (t, f, ty);
        }
        if t.ty == never {
            let ty = f.ty;
            return (t, f, ty);
        }
        if f.ty == never {
            let ty = t.ty;
            return (t, f, ty);
        }
        if t.ty == error || f.ty == error {
            return (t, f, error);
        }
        let tt = t.ty;
        let f = match self.try_coerce(f, tt) {
            Ok(f) => return (t, f, tt),
            Err(f) => f,
        };
        let ft = f.ty;
        let t = match self.try_coerce(t, ft) {
            Ok(t) => return (t, f, ft),
            Err(t) => t,
        };
        if let Some(e) = exp.filter(|e| *e != error) {
            let t = self.coerce(t, e);
            let f = self.coerce(f, e);
            return (t, f, e);
        }
        self.report_mismatch(tt, &f);
        (t, f, tt)
    }
}

/// `x`, `this.a`, `x.a.b`: re-reading it has no side effects.
/// A place that reading again has no effect: names, `this`, member paths, and indexes by such
/// values, literals and arithmetic on them (`xs[i + 1].count`).
fn is_pure_place(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Index {
            object,
            index,
            optional: false,
        } => is_pure_place(object) && is_pure_value(index),
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_pure_place(object),
        ast::ExprKind::Paren(inner) => is_pure_place(inner),
        _ => is_plain_path(e),
    }
}

fn is_pure_value(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(_) => true,
        ast::ExprKind::Unary { expr, .. } => is_pure_value(expr),
        ast::ExprKind::Binary { lhs, rhs, .. } => is_pure_value(lhs) && is_pure_value(rhs),
        _ => is_pure_place(e),
    }
}

fn is_plain_path(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_plain_path(object),
        ast::ExprKind::Paren(inner) => is_plain_path(inner),
        _ => false,
    }
}
