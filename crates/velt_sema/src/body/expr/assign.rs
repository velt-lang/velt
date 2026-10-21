//! Assignment, compound assignment, `++`/`--` and the ternary operator. Targets that name a
//! setter are handed to [`setters`](super::setters).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::ops::{hir_binop, untyped};
use crate::body::narrow::{nothing_word, only_nothings};
use crate::body::places::set_place_mode;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, BinOp, ExprKind as H, Nothing, TyId, UseMode};

/// What an assignment writes to.
enum AssignTarget {
    /// A writable place (`BorrowMut` modes).
    Place(hir::Expr),
    /// `obj.name` where `name` is a setter, or a field written through an interface
    /// (`iface_write`): the checked receiver.
    Setter(hir::Expr),
    /// `r[k]` / `r.name` of a `Record`: the checked record.
    Record(hir::Expr),
    /// `xs[i]` of a std class with `__index` / `__setIndex` (`indexers`): the checked object.
    Indexer(hir::Expr),
    /// `x.f` / `x[k]` of an `any` (`dynamic_write`): the checked receiver.
    Dynamic(hir::Expr),
    /// `x[k]` of a type parameter's value by a key known per instantiation (`key_index.rs`).
    Keyed(hir::Expr, hir::Expr),
}

use super::assign_paths::{is_plain_path, is_pure_place};

impl FnCx<'_, '_> {
    /// Resolve an assignment target to a writable place or a setter. Reports errors.
    fn assign_target(&mut self, target: &ast::Expr, span: Span) -> Option<AssignTarget> {
        if self.reject_env_assign(target) || self.reject_argv_assign(target) {
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
                let obj = match self.dynamic_write_receiver(obj) {
                    Ok(d) => return d.map(AssignTarget::Dynamic),
                    Err(obj) => obj,
                };
                if self.record_args(obj.ty).is_some() {
                    return Some(AssignTarget::Record(obj));
                }
                self.note_field_write(obj.ty, prop);
                if let Some(ok) = self.iface_write_target(obj.ty, prop) {
                    return ok.then_some(AssignTarget::Setter(obj));
                }
                if self.has_setter(obj.ty, &prop.name) {
                    return Some(AssignTarget::Setter(obj));
                }
                if self.reject_getter_assign(obj.ty, prop) {
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
                let obj = match self.dynamic_write_receiver(obj) {
                    Ok(d) => {
                        if d.is_none() {
                            self.expr(index, None, Want::Borrow);
                        }
                        return d.map(AssignTarget::Dynamic);
                    }
                    Err(obj) => obj,
                };
                if self.record_args(obj.ty).is_some() {
                    return Some(AssignTarget::Record(obj));
                }
                if self.has_indexer(obj.ty)
                    && super::member::literal_key(index).is_none()
                    && self.symbol_key_of(index).is_none()
                {
                    return Some(AssignTarget::Indexer(obj));
                }
                // `c["v"] = x` with a constant key is `c.v = x`, and `c[KEY] = x` with a symbol
                // key the member it names: a setter or a getter-only accessor is handled the
                // same way.
                let key = match self.symbol_key_of(index) {
                    Some(key) => Some(key),
                    None => self.single_key(index),
                };
                if let Some(name) = key {
                    let prop = ast::Ident {
                        name,
                        span: index.span,
                    };
                    // A field write for the copies of `crate::object_copies`, as for `o.k = v`.
                    self.note_field_write(obj.ty, &prop);
                    if let Some(ok) = self.iface_write_target(obj.ty, &prop) {
                        return ok.then_some(AssignTarget::Setter(obj));
                    }
                    if self.has_setter(obj.ty, &prop.name) {
                        return Some(AssignTarget::Setter(obj));
                    }
                    if self.reject_getter_assign(obj.ty, &prop) {
                        return None;
                    }
                }
                let obj = match self.generic_key_target(obj, index) {
                    Ok(k) => return k.map(|(obj, key)| AssignTarget::Keyed(obj, key)),
                    Err(obj) => obj,
                };
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

    fn assign_local(&mut self, id: &ast::Ident, span: Span) -> Option<hir::Expr> {
        if self.untyped_use(id, true) {
            return None;
        }
        let Some(l) = self.lookup_local(&id.name, id.span) else {
            let item = self.lookup_item(&id.name, id.span);
            if let Some(place) = self.module_let_target(item, id) {
                return place;
            } else if item.is_some() {
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

    /// A read of a place that was built for writing (compound assignment / `++` value).
    fn place_read(&mut self, place: &hir::Expr, want: Want) -> hir::Expr {
        let mut r = place.clone();
        set_place_mode(&mut r, self.use_mode(place.ty, want));
        r
    }

    /// An assignment whose value is used (`(line = next()) != null`): the assignment, then the
    /// target read again; a statement where no value is wanted (`void` expected). Other targets
    /// than variables or fields of them would be evaluated twice: not supported as values.
    pub(crate) fn assign_value(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let wanted = exp != Some(self.cx.ty.unit);
        let (a, valued) = self.assign_as(op, target, value, wanted, span);
        if valued || !wanted || self.cx.ty.is_bottom(a.ty) {
            return a;
        }
        if !is_plain_path(target) {
            self.cx
                .err(super::assign_pattern::value_use_message(target), span);
            return self.error_expr(span);
        }
        let v = self.expr(target, None, Want::Borrow);
        let v = self.nullish_assigned_read(op, &a, v, span);
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
        self.assign_as(op, target, value, false, span).0
    }

    /// [`Self::assign`]; with `as_value`, an accessor's read-modify-write also gives the value
    /// (returned `true`); a destructuring target is `assign_pattern`'s.
    fn assign_as(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> (hir::Expr, bool) {
        if let Some(h) = self.pattern_assign(op, target, value, span) {
            return (h, false);
        }
        if let Some(h) = self.keyed_assign(op, target, value, span) {
            return (h, false);
        }
        if let Some(op @ (ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish)) = op {
            let checked = self.assign_target(target, span);
            if let Some(AssignTarget::Setter(obj)) = checked {
                let e = self.setter_assign(obj, Some(op), target, value, as_value, span);
                return (e, as_value);
            }
            if let Some(AssignTarget::Indexer(obj)) = checked {
                let e = self.indexer_assign(obj, Some(op), target, value, as_value, span);
                return (e, as_value);
            }
            if let Some(AssignTarget::Dynamic(obj)) = checked {
                return self.dynamic_assign(obj, Some(op), target, value, as_value, span);
            }
            // A record key has rules of its own (a key may be missing; `record_logical.rs`).
            if let Some(AssignTarget::Record(obj)) = checked {
                let (object, key) = super::record::record_parts(target);
                let e = self.record_logical(obj, object, key, op, value, as_value, span);
                return (e, as_value);
            }
            (self.logical_assign(op, checked, target, value, span), false)
        } else {
            self.plain_or_compound_assign(op, target, value, as_value, span)
        }
    }

    /// `a ??= b` on a place (`checked` is the target as resolved; not a record key).
    fn logical_assign(
        &mut self,
        op: ast::BinaryOp,
        checked: Option<AssignTarget>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
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
        // A target in error is reported once, here.
        match checked {
            None => {
                self.expr(value, None, Want::Borrow);
                return self.error_expr(span);
            }
            Some(
                AssignTarget::Place(_)
                | AssignTarget::Record(_)
                | AssignTarget::Setter(_)
                | AssignTarget::Indexer(_)
                | AssignTarget::Dynamic(_)
                | AssignTarget::Keyed(..),
            ) => {}
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
        self.assign(None, target, &rhs, span)
    }

    /// `target = value` / `target op= value` (`op` not logical).
    fn plain_or_compound_assign(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> (hir::Expr, bool) {
        let e = match self.assign_target(target, span) {
            Some(AssignTarget::Place(place)) => self.place_assign(place, op, target, value, span),
            Some(AssignTarget::Setter(obj)) => {
                let e = self.setter_assign(obj, op, target, value, as_value, span);
                return (e, as_value);
            }
            Some(AssignTarget::Indexer(obj)) => {
                let e = self.indexer_assign(obj, op, target, value, as_value, span);
                return (e, as_value);
            }
            Some(AssignTarget::Record(obj)) => {
                let (object, key) = super::record::record_parts(target);
                self.record_assign(obj, object, key, op, target, value, span)
            }
            Some(AssignTarget::Dynamic(obj)) => {
                return self.dynamic_assign(obj, op, target, value, as_value, span);
            }
            Some(AssignTarget::Keyed(obj, key)) => self.key_write_of(obj, key, op, value, span),
            None => {
                self.expr(value, None, Want::Borrow);
                self.error_expr(span)
            }
        };
        (e, false)
    }

    /// `place = value` / `place op= value` on a writable place.
    fn place_assign(
        &mut self,
        mut place: hir::Expr,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let unit = self.cx.ty.unit;
        if op.is_some() && self.untyped_update(&place, span) {
            self.expr(value, None, Want::Borrow);
            return self.error_expr(span);
        }
        let first = match op {
            None => self.first_assign(&mut place, value),
            Some(_) => None,
        };
        let lty = place.ty;
        let Some(op) = op else {
            let v = match first {
                Some(v) => v,
                None => {
                    let v = self.expr(value, Some(lty), Want::Move);
                    // A local declared from a literal takes the type of what it is assigned
                    // (`literal_locals`); other places are a typed position.
                    if self.literal_assign(&place, &v) {
                        self.try_coerce(v, lty).unwrap_or_else(|v| {
                            self.report_mismatch(lty, &v);
                            v
                        })
                    } else {
                        self.coerce(v, lty)
                    }
                }
            };
            self.unnarrow_fields(target);
            if let H::Local(l, _) = place.kind {
                self.check_capture_assign(l, target.span);
                self.unnarrow(l);
            }
            // Assigning narrows the target like a check would (TS): to non-null, or to the
            // union member assigned (`assign_narrow`).
            let token = match place.kind {
                H::Local(l, _) => Some(l),
                _ if matches!(v.kind, H::WrapSome(_)) => self.field_token(target),
                _ => None,
            };
            if let Some(t) = token.filter(|_| !crate::body::assign_narrow::is_cast(value)) {
                self.narrow_to_assigned(t, lty, &v);
            }
            let kind = H::Assign {
                place: Box::new(place),
                value: Box::new(v),
            };
            return self.mk(kind, unit, span);
        };
        if lty == self.cx.ty.str_ && op == ast::BinaryOp::Add {
            // `rows[f()].out += x` / `g().out += x`: the object and indices are evaluated once,
            // before `x`, as in JS (`ns[f()] += 1` on numbers is one `CompoundAssign`).
            let mut place = place;
            let mut stmts = Vec::new();
            self.hoist_indices(&mut place, &mut stmts);
            // `s += n`: the number is appended as `String(n)` writes it (#740).
            let v = self.expr(value, Some(lty), Want::Borrow);
            // `s += a` with an `any` `a` appends `String(a)`, as `s + a` does (not a checked
            // conversion to `string`).
            let v = match self.any_to_source(v) {
                Ok(a) => self.implicit_any_text(a, "a string concatenation"),
                Err(v) => self.concat_operand(v),
            };
            let v = self.coerce(v, lty);
            let mut cur = self.place_read(&place, Want::Borrow);
            if crate::effects::may_change_memory(&v) && Self::through_element(&place) {
                // The right-hand side may change or move what holds the string (`rows[0].out +=
                // grow(rows)`): take the current value first, as JS reads it before `v` (#580).
                // A variable or field path is appended in place by lowering, which keeps its old
                // value alive itself.
                cur = self.hoist_value(cur, &mut stmts);
            }
            let cat = self.concat(cur, v, span);
            let kind = H::Assign {
                place: Box::new(place),
                value: Box::new(cat),
            };
            let assign = self.mk(kind, unit, span);
            return self.with_temps(stmts, assign);
        }
        let v = self.expr(value, Some(lty), Want::Borrow);
        let v = self.unbrand(v);
        self.literal_operands(op, &place, &v);
        // `x |= v` and the other bitwise assignments on numbers: `x = x | v` (`int32.rs`).
        let v = match self.js_bitwise_operands(op, &place, v) {
            Ok(v) => {
                // The index of `xs[next()] |= 1` is evaluated once, before the value.
                let mut place = place;
                let mut stmts = Vec::new();
                self.hoist_indices(&mut place, &mut stmts);
                let cur = self.place_read(&place, Want::Borrow);
                let value = self.js_bitwise_assign(op, cur, v, span);
                let kind = H::Assign {
                    place: Box::new(place),
                    value: Box::new(value),
                };
                let assign = self.mk(kind, unit, span);
                return self.with_temps(stmts, assign);
            }
            Err(v) => v,
        };
        let v = self.compound_operand(&place, v);
        if self.check_operands(op, lty, &v, span).is_none() {
            return self.error_expr(span);
        }
        let bop = hir_binop(op).expect("ICE: logical compound op");
        let mut place = place;
        let mut stmts = Vec::new();
        if Self::has_value_object(&place) {
            self.hoist_indices(&mut place, &mut stmts);
        }
        let kind = H::CompoundAssign {
            op: bop,
            place: Box::new(place),
            value: Box::new(v),
        };
        let assign = self.mk(kind, unit, span);
        self.with_temps(stmts, assign)
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
        if let Some(h) = self.keyed_update(op, prefix, target, as_value, span) {
            return h;
        }
        let place = match self.assign_target(target, span) {
            Some(AssignTarget::Place(place)) => place,
            Some(AssignTarget::Setter(obj)) => {
                return self.setter_update(obj, op, prefix, target, as_value, span)
            }
            Some(AssignTarget::Record(obj)) => {
                return self.record_update(obj, op, target, as_value, span)
            }
            Some(AssignTarget::Indexer(obj)) => {
                return self.indexer_update(obj, op, prefix, target, as_value, span)
            }
            Some(AssignTarget::Dynamic(_)) => {
                self.dynamic_write_error(span);
                return self.error_expr(span);
            }
            Some(AssignTarget::Keyed(obj, _)) => {
                self.cannot_index(&obj);
                return self.error_expr(span);
            }
            None => return self.error_expr(span),
        };
        if self.untyped_update(&place, span) {
            return self.error_expr(span);
        }
        let lty = place.ty;
        self.literal_use_arith(&place);
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
        // `f().n++`, and `xs[f()]++` as a value (read, then updated): evaluated once.
        let mut place = place;
        let mut stmts = Vec::new();
        if as_value || Self::has_value_object(&place) {
            self.hoist_indices(&mut place, &mut stmts);
        }
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
            return self.with_temps(stmts, ca);
        }
        let value = self.update_value(ca, read, prefix, span);
        self.with_temps_value(stmts, value)
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
        let exp = exp.or_else(|| self.nothings_conditional_ty(then, els));
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
        let late = |e: &ast::Expr| untyped(e) || only_nothings(e);
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
        if let Some(n) = written_nothings(second) {
            match self.cx.ty.nothing_of(first.ty) {
                None => return Some(self.cx.ty.option_of(first.ty, n)),
                // `c ? undefined : x` against a `T | null` (or the reverse): both nothings
                // (#1218).
                Some(m) if !n.within(m) => {
                    let core = self.cx.ty.core(first.ty).unwrap_or(first.ty);
                    return Some(self.cx.ty.option_of(core, m.join(n)));
                }
                Some(_) => {}
            }
        }
        Some(first.ty)
    }

    /// Common type of two branches (coercing one to the other, or both to `exp`), else their
    /// union, as in TypeScript (`qty ? qty : "none"` is a `number | string`).
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
        let unit = self.cx.ty.unit;
        if tt == unit || ft == unit {
            self.report_mismatch(tt, &f);
            return (t, f, tt);
        }
        let span = t.span.to(f.span);
        self.union_join(t, f, span)
    }
}

/// The `null` / `undefined` literals `e` may give: itself, or the arms of a conditional
/// (`c ? null : undefined`), so the branch is checked against a type holding them.
fn written_nothings(e: &ast::Expr) -> Option<Nothing> {
    match &e.kind {
        ast::ExprKind::Paren(inner) => written_nothings(inner),
        ast::ExprKind::Cond { then, els, .. } => {
            match (written_nothings(then), written_nothings(els)) {
                (Some(a), Some(b)) => Some(a.join(b)),
                (a, b) => a.or(b),
            }
        }
        _ => nothing_word(e).map(|w| match w {
            "null" => Nothing::Null,
            _ => Nothing::Undefined,
        }),
    }
}
