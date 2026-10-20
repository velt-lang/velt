//! A write into a part of a field read through an interface (`x.t[0] = 5`, `x.o.t[1] += 1`,
//! `x.t[0]++`, where `x` is an interface value or a bounded generic and `t` a field of the
//! interface). The getter slot gives an owned value, so a tuple or another value-type part
//! written in place would change a temporary copy and the write would be lost (JS changes the
//! array the object holds). The write goes through the setter slot instead:
//!
//! ```text
//! { const <receiver> = x;        // only when evaluating `x` again could differ
//!   const <rhs> = f();           // a right-hand side with an effect (a call, `await`)
//!   let <part> = <receiver>.t;   // getter
//!   <part>[0] = <rhs>;           // the original write, on the copy
//!   <receiver>.t = <part>;       // setter
//!   <value> }                    // the assignment's value, where one is wanted
//! ```
//!
//! The field is read after the right-hand side ran (and after its `await`s): JS writes into the
//! array the object holds when the element is assigned, so a change the right-hand side (or
//! another task) makes to the field meanwhile is kept. A compound right-hand side with an
//! effect becomes `<receiver>.t[0] op f()`, which reads the old element first, as JS does.
//!
//! A receiver that cannot be checked an extra time to learn its type (`xs[i++].t[0]`,
//! `(c ? a : b).t[0]`, `xs.find(…)!.t[0]`, `fs[0]().t[0]`) is bound to a temporary first,
//! with every index in the target that has an effect, in evaluation order, when its field
//! names a field that an interface's read may copy. `x["t"]` is `x.t`.
//!
//! The setter makes every implementor writable (`iface_writes`), so an implementor that is a
//! value type becomes a counted object and every holder of the interface value sees the write.
//! Parts that are references (arrays, class objects, maps) are written in place as before. A
//! `readonly` field has a setter slot for this write-back only (TS `readonly` is shallow: the
//! field's parts may change); assigning the field itself stays an error.

use velt_common::Span;
use velt_syntax::ast;

use super::accessor_rmw::{block, stmt, RECEIVER};
use super::assign_paths::{is_plain_path, is_pure_value};
use super::member_keys::{literal_key, member_view};
use super::setters::synth;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H};

const PART: &str = "<iface part>";
const VALUE: &str = "<iface value>";
const RHS: &str = "<iface rhs>";

/// The write made on the copied part.
#[derive(Clone, Copy)]
pub(super) enum NestedWrite<'a> {
    /// `target = value` / `target op= value`.
    Assign(Option<ast::BinaryOp>, &'a ast::Expr),
    /// `target++` / `--target`; `prefix`: the value is the new one.
    Update(ast::UpdateOp, bool),
}

/// `e` without parentheses.
fn bare(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(inner) => bare(inner),
        _ => e,
    }
}

/// The object a member or element target is part of.
fn inner(e: &ast::Expr) -> Option<&ast::Expr> {
    match &bare(e).kind {
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        }
        | ast::ExprKind::Index {
            object,
            optional: false,
            ..
        } => Some(object),
        _ => None,
    }
}

/// Can `e` be checked an extra time to learn its type? Variables, `this`, members, elements at
/// plain indices and calls of named functions with plain arguments: nothing in it creates a
/// closure or another definition.
fn peekable(e: &ast::Expr) -> bool {
    match &bare(e).kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member { object, .. } => peekable(object),
        ast::ExprKind::Index { object, index, .. } => peekable(object) && is_pure_value(index),
        ast::ExprKind::Call { callee, args, .. } => {
            is_plain_path(callee) && args.iter().all(is_pure_value)
        }
        _ => false,
    }
}

/// `target` with the part at `depth` levels below it replaced by `with`.
fn replace(target: &ast::Expr, depth: usize, with: &ast::Expr) -> ast::Expr {
    if depth == 0 {
        return with.clone();
    }
    let t = bare(target);
    let kind = match &t.kind {
        ast::ExprKind::Member {
            object,
            prop,
            optional,
        } => ast::ExprKind::Member {
            object: Box::new(replace(object, depth - 1, with)),
            prop: prop.clone(),
            optional: *optional,
        },
        ast::ExprKind::Index {
            object,
            index,
            optional,
        } => ast::ExprKind::Index {
            object: Box::new(replace(object, depth - 1, with)),
            index: index.clone(),
            optional: *optional,
        },
        _ => unreachable!("ICE: replaced part is below a member or element target"),
    };
    ast::Expr {
        id: t.id,
        kind,
        span: t.span,
    }
}

/// Are the indexes of `target` down to the part `depth` levels below it free of effects (so
/// the element may be read before it is written)?
fn pure_below(target: &ast::Expr, depth: usize) -> bool {
    if depth == 0 {
        return true;
    }
    match &bare(target).kind {
        ast::ExprKind::Member { object, .. } => pure_below(object, depth - 1),
        ast::ExprKind::Index { object, index, .. } => {
            is_pure_value(index) && pure_below(object, depth - 1)
        }
        _ => false,
    }
}

/// A name in scope (a temporary of this module).
fn ident(name: &str, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Ident(ast::Ident {
            name: name.into(),
            span,
        }),
        span,
    )
}

impl FnCx<'_, '_> {
    /// The write `w` to `target`, when `target` lies inside a value-type field read through an
    /// interface (module docs). `None`: it does not; the write is checked as usual.
    pub(super) fn iface_nested_write(
        &mut self,
        target: &ast::Expr,
        w: NestedWrite,
        as_value: bool,
        span: Span,
    ) -> Option<(hir::Expr, bool)> {
        if let Some(r) = self.hoisted_write(target, w, as_value, span) {
            return Some(r);
        }
        let (depth, object, prop) = self.iface_part_of(target)?;
        let obj = self.expr(object, None, Want::Borrow);
        let f = self.iface_field_of(obj.ty, &prop.name)?;
        if !self.iface_part_target(&f, &prop) {
            return Some((self.error_expr(span), false));
        }
        let effect = match w {
            NestedWrite::Assign(_, v) if !is_pure_value(v) => Some(v),
            _ => None,
        };
        let mut stmts = vec![];
        self.push_scope();
        let recv = self.receiver_once(obj, object, effect, &mut stmts);
        let rhs;
        let mut w = w;
        if let (NestedWrite::Assign(op, v), Some(_)) = (w, effect) {
            // The receiver is the temporary when one was bound.
            let recv_ast = match stmts.is_empty() {
                true => object.clone(),
                false => ident(RECEIVER, object.span),
            };
            let read = replace(target, depth + 1, &recv_ast);
            if let Some(r) = self.value_first(op, &read, depth, v, &mut stmts) {
                rhs = r;
                w = NestedWrite::Assign(None, &rhs);
            }
        }
        let read = match self.getter_read(recv.clone(), &prop, span) {
            Ok(read) => read,
            Err(_) => {
                self.pop_scope();
                return Some((self.error_expr(span), false));
            }
        };
        let (pty, pspan) = (read.ty, read.span);
        let part = self.new_local(PART, pty, true, pspan, LocalKind::Temp);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(PART.to_string(), part);
        stmts.push(stmt(
            hir::StmtKind::Let {
                local: part,
                init: Some(read),
            },
            pspan,
        ));
        let part_ref = ident(PART, pspan);
        let target = replace(target, depth, &part_ref);
        let (write, valued) = match w {
            NestedWrite::Assign(op, value) => {
                self.assign_or_value(op, &target, value, as_value, span)
            }
            NestedWrite::Update(op, prefix) => {
                (self.update(op, prefix, &target, as_value, span), as_value)
            }
        };
        let (wty, unit) = (write.ty, self.cx.ty.unit);
        let value = if valued && wty != unit && !self.cx.ty.is_bottom(wty) {
            let l = self.new_local(VALUE, wty, false, span, LocalKind::Temp);
            stmts.push(stmt(
                hir::StmtKind::Let {
                    local: l,
                    init: Some(write),
                },
                span,
            ));
            Some(self.read_temp(l, wty, span))
        } else {
            stmts.push(stmt(hir::StmtKind::Expr(write), span));
            None
        };
        let call = self.setter_call(recv, &prop, &part_ref, span);
        stmts.push(stmt(hir::StmtKind::Expr(call), span));
        self.pop_scope();
        let ty = value.as_ref().map_or(unit, |v| v.ty);
        let valued = value.is_some();
        Some((
            self.mk(H::Block(block(stmts, value, span)), ty, span),
            valued,
        ))
    }

    /// `const <rhs> = v` (`= read op v` for a compound `op`), checked as the type of the part
    /// `read` names: the right-hand side runs before the field is read (module docs). `None`:
    /// the part cannot be read first (an index with an effect); the write keeps its order.
    fn value_first(
        &mut self,
        op: Option<ast::BinaryOp>,
        read: &ast::Expr,
        depth: usize,
        v: &ast::Expr,
        stmts: &mut Vec<hir::Stmt>,
    ) -> Option<ast::Expr> {
        if !pure_below(read, depth) {
            return None;
        }
        // Checked again where it is written: its errors are reported there.
        let mark = self.cx.diags.len();
        let ty = self.expr(read, None, Want::Borrow).ty;
        self.cx.diags.truncate(mark);
        if self.cx.ty.is_bottom(ty) || self.cx.ty.has_error(ty) {
            return None;
        }
        let value = match op {
            None => v.clone(),
            Some(op) => synth(
                ast::ExprKind::Binary {
                    op,
                    lhs: Box::new(read.clone()),
                    rhs: Box::new(v.clone()),
                },
                v.span,
            ),
        };
        let h = self.expr_coerce(&value, ty, Want::Move);
        self.bind_temp(RHS, h, stmts);
        Some(ident(RHS, v.span))
    }

    /// The write `w` to `target` with its receiver and the indexes in it that have an effect
    /// bound to temporaries first (module docs), when a part of it may be a copied interface
    /// field on a receiver that cannot be checked an extra time.
    fn hoisted_write(
        &mut self,
        target: &ast::Expr,
        w: NestedWrite,
        as_value: bool,
        span: Span,
    ) -> Option<(hir::Expr, bool)> {
        if !self.needs_hoist(target) {
            return None;
        }
        let mut stmts = vec![];
        self.push_scope();
        let target = self.hoist(target, true, false, &mut stmts);
        let (h, valued) = match w {
            NestedWrite::Assign(op, v) => self.assign_as(op, &target, v, as_value, span),
            NestedWrite::Update(op, prefix) => {
                (self.update(op, prefix, &target, as_value, span), as_value)
            }
        };
        self.pop_scope();
        let unit = self.cx.ty.unit;
        let valued = valued && h.ty != unit;
        let ty = if valued { h.ty } else { unit };
        let b = if valued {
            block(stmts, Some(h), span)
        } else {
            stmts.push(stmt(hir::StmtKind::Expr(h), span));
            block(stmts, None, span)
        };
        Some((self.mk(H::Block(b), ty, span), valued))
    }

    /// Does a part of `target` name a field that an interface's read may copy, on a receiver
    /// that cannot be checked an extra time?
    fn needs_hoist(&mut self, target: &ast::Expr) -> bool {
        let mut cur = inner(target);
        while let Some(c) = cur {
            if let Some((object, prop)) = member_view(bare(c)) {
                if !peekable(object)
                    && self.cx.copied_field_name(&prop.name)
                    && !self.object_receiver(object)
                {
                    return true;
                }
            }
            cur = inner(c);
        }
        false
    }

    /// Is `e` (a receiver that cannot be checked an extra time) known to be a class or object
    /// type value from its form alone (`x as O`, `xs[0]!`, `o!`)? Its fields are places, so the
    /// write takes the normal path.
    fn object_receiver(&mut self, e: &ast::Expr) -> bool {
        let Some(t) = self.receiver_ty(e) else {
            return false;
        };
        let t = self.cx.ty.core(t).unwrap_or(t);
        self.adt_of(t).is_some()
    }

    /// The type of receiver `e` where its form gives it without checking `e` again: an `as`
    /// type, or a non-null assertion of a receiver that may be checked again.
    fn receiver_ty(&mut self, e: &ast::Expr) -> Option<hir::TyId> {
        // Checked again where it is used: its errors are reported there.
        let mark = self.cx.diags.len();
        let t = match &bare(e).kind {
            ast::ExprKind::Cast { ty, .. } => Some(self.resolve(ty)),
            ast::ExprKind::NonNull(inner) if peekable(inner) => {
                Some(self.expr(inner, None, Want::Borrow).ty)
            }
            ast::ExprKind::NonNull(inner) => self.receiver_ty(inner),
            _ => None,
        };
        self.cx.diags.truncate(mark);
        t.filter(|&t| !self.cx.ty.has_error(t))
    }

    /// `e` (a part of the target; `top`: the target itself) with its receiver and the indexes
    /// below it that have an effect bound to temporaries (appended to `stmts`), in evaluation
    /// order. `strict`: also a receiver that may be checked again but not evaluated again (a
    /// call).
    fn hoist(
        &mut self,
        e: &ast::Expr,
        top: bool,
        strict: bool,
        stmts: &mut Vec<hir::Stmt>,
    ) -> ast::Expr {
        let b = bare(e);
        let kind = match &b.kind {
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } => ast::ExprKind::Member {
                object: Box::new(self.hoist(object, false, strict, stmts)),
                prop: prop.clone(),
                optional: false,
            },
            ast::ExprKind::Index {
                object,
                index,
                optional: false,
            } => {
                let object = Box::new(self.hoist(object, false, strict, stmts));
                let index = if top || is_pure_value(index) || literal_key(index).is_some() {
                    index.clone()
                } else {
                    Box::new(self.bind_hoisted(index, stmts))
                };
                ast::ExprKind::Index {
                    object,
                    index,
                    optional: false,
                }
            }
            _ if top || (!strict && peekable(b)) || is_plain_path(b) => return e.clone(),
            _ => return self.bind_hoisted(e, stmts),
        };
        ast::Expr {
            id: b.id,
            kind,
            span: b.span,
        }
    }

    /// `target = value` / `target op= value` used as a value, on an element or a member of
    /// something other than a variable (`(c.t[0] = 5)`, `(xs[f()] += 1)`): the receiver and
    /// every index with an effect (the target's own too) bound to temporaries in evaluation
    /// order, the assignment, then the target, now free of effects, read again. `None`: not an
    /// element or member target.
    pub(super) fn element_assign_value(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let t = bare(target);
        let mut stmts = vec![];
        self.push_scope();
        let t = match &t.kind {
            ast::ExprKind::Index {
                object,
                index,
                optional: false,
            } => {
                let object = Box::new(self.hoist_held(object, &mut stmts));
                let index = match is_pure_value(index) || literal_key(index).is_some() {
                    true => index.clone(),
                    false => Box::new(self.bind_hoisted(index, &mut stmts)),
                };
                let kind = ast::ExprKind::Index {
                    object,
                    index,
                    optional: false,
                };
                ast::Expr {
                    id: t.id,
                    kind,
                    span: t.span,
                }
            }
            ast::ExprKind::Member {
                optional: false, ..
            } => self.hoist(t, true, true, &mut stmts),
            _ => {
                self.pop_scope();
                return None;
            }
        };
        let (a, valued) = self.assign_as(op, &t, value, true, span);
        let (unit, ty) = (self.cx.ty.unit, a.ty);
        let value = if valued || self.cx.ty.is_bottom(ty) || self.cx.ty.has_error(ty) {
            a
        } else {
            stmts.push(stmt(hir::StmtKind::Expr(a), span));
            let read = self.assigned_read(&t);
            self.expr(&read, None, Want::Borrow)
        };
        self.pop_scope();
        let ty = if value.ty == unit { unit } else { value.ty };
        Some(self.mk(H::Block(block(stmts, Some(value), span)), ty, span))
    }

    /// [`Self::hoist`] of the receiver of an element assigned as a value, seeing through `!` and
    /// `as`: `p!` and `p as T` name the place `p` names, so a tuple held there stays the place
    /// written (`(opt![0] = 5)`, `((u as [number, string])[0] = 9)`), not a copy bound to a
    /// temporary.
    fn hoist_held(&mut self, e: &ast::Expr, stmts: &mut Vec<hir::Stmt>) -> ast::Expr {
        let b = bare(e);
        let kind = match &b.kind {
            ast::ExprKind::NonNull(x) => {
                ast::ExprKind::NonNull(Box::new(self.hoist_held(x, stmts)))
            }
            ast::ExprKind::Cast { expr, ty } => ast::ExprKind::Cast {
                expr: Box::new(self.hoist_held(expr, stmts)),
                ty: ty.clone(),
            },
            _ => return self.hoist(e, false, true, stmts),
        };
        ast::Expr {
            id: b.id,
            kind,
            span: b.span,
        }
    }

    /// `t` (a pure target just assigned) read again: a record key is present now, so `r[k]` is
    /// `r[k]!` unless the record's values may be `undefined` themselves.
    fn assigned_read(&mut self, t: &ast::Expr) -> ast::Expr {
        let Some(object) = inner(t) else {
            return t.clone();
        };
        let mark = self.cx.diags.len();
        let ot = self.expr(object, None, Want::Borrow).ty;
        self.cx.diags.truncate(mark);
        match self.record_args(ot) {
            Some((_, v)) if self.cx.ty.nothing_of(v).is_none() => {
                synth(ast::ExprKind::NonNull(Box::new(t.clone())), t.span)
            }
            _ => t.clone(),
        }
    }

    /// `let <iface hoisted k> = e` (appended to `stmts`), named by the result.
    fn bind_hoisted(&mut self, e: &ast::Expr, stmts: &mut Vec<hir::Stmt>) -> ast::Expr {
        let h = self.expr(e, None, Want::Move);
        let name = format!("<iface hoisted {}>", stmts.len());
        let (ty, span) = (h.ty, h.span);
        let l = self.new_local(&name, ty, true, span, LocalKind::Temp);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(name.clone(), l);
        stmts.push(stmt(
            hir::StmtKind::Let {
                local: l,
                init: Some(h),
            },
            span,
        ));
        ident(&name, e.span)
    }

    /// `(depth, object, prop)`: the outermost part `object.prop` (or `object["prop"]`) strictly
    /// inside `target` that is a field read through an interface whose value a write in place
    /// would not reach.
    fn iface_part_of<'e>(
        &mut self,
        target: &'e ast::Expr,
    ) -> Option<(usize, &'e ast::Expr, ast::Ident)> {
        let mut cur = inner(target)?;
        let mut depth = 1;
        loop {
            if let Some((object, prop)) = member_view(bare(cur)) {
                if peekable(object) && self.copied_iface_part(object, &prop) {
                    return Some((depth, object, prop));
                }
            }
            cur = inner(cur)?;
            depth += 1;
        }
    }

    /// Is `object.prop` a field read through an interface whose value is copied by the read (a
    /// value type: a tuple, an object type, an option or union of them)?
    fn copied_iface_part(&mut self, object: &ast::Expr, prop: &ast::Ident) -> bool {
        // Checked again where it is used: its errors are reported there.
        let mark = self.cx.diags.len();
        let t = self.expr(object, None, Want::Borrow).ty;
        self.cx.diags.truncate(mark);
        let Some(f) = self.iface_field_of(t, &prop.name) else {
            return false;
        };
        self.cx.copied_part(f.ty, false)
    }

    /// [`Self::assign`] that also gives the assignment's value where it is wanted and the
    /// target allows one (`true`: the expression is that value).
    fn assign_or_value(
        &mut self,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> (hir::Expr, bool) {
        if !as_value {
            return (self.assign(op, target, value, span), false);
        }
        // The value of `p[0] = v` is `v` converted to the element's type: the assignment, then
        // the element read again (the part is a local, nothing else changes it).
        let a = self.assign(op, target, value, span);
        let v = self.expr(target, None, Want::Borrow);
        let ty = v.ty;
        let b = block(vec![stmt(hir::StmtKind::Expr(a), span)], Some(v), span);
        (self.mk(H::Block(b), ty, span), true)
    }
}
