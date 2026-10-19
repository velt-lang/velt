//! A write into a part of a field read through an interface (`x.t[0] = 5`, `x.o.t[1] += 1`,
//! `x.t[0]++`, where `x` is an interface value or a bounded generic and `t` a field of the
//! interface). The getter slot gives an owned value, so a tuple or another value-type part
//! written in place would change a temporary copy and the write would be lost (JS changes the
//! array the object holds). The write goes through the setter slot instead:
//!
//! ```text
//! { const <receiver> = x;        // only when evaluating `x` again could differ
//!   let <part> = <receiver>.t;   // getter
//!   <part>[0] = 5;               // the original write, on the copy
//!   <receiver>.t = <part>;       // setter
//!   <value> }                    // the assignment's value, where one is wanted
//! ```
//!
//! The setter makes every implementor writable (`iface_writes`), so an implementor that is a
//! value type becomes a counted object and every holder of the interface value sees the write.
//! Parts that are references (arrays, class objects, maps) are written in place as before. A
//! `readonly` field has no setter slot: changing a part of one through an interface is an error.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::accessor_rmw::{block, stmt};
use super::assign_paths::{is_plain_path, is_pure_value};
use super::setters::synth;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, TyKind};

const PART: &str = "<iface part>";
const VALUE: &str = "<iface value>";

/// The write made on the copied part.
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
        let (depth, object, prop) = self.iface_part_of(target)?;
        let later = match w {
            NestedWrite::Assign(_, v) => Some(v),
            NestedWrite::Update(..) => None,
        };
        // The field is written back through the setter slot, which a `readonly` field has not:
        // an error rather than a write to a copy.
        let obj = self.expr(object, None, Want::Borrow);
        let f = self.iface_field_of(obj.ty, &prop.name)?;
        if f.readonly {
            let name = self.cx.shown_name(&prop.name);
            self.cx.error(
                Diagnostic::error(
                    format!("changing a part of `{name}` through an interface or generic value is not supported yet: it is a readonly field"),
                    target.span,
                )
                .with_note(format!("make the field writable in `{}`: the changed part is then written back through it", self.cx.display(obj.ty))),
            );
            return Some((self.error_expr(span), false));
        }
        if self.iface_write_target(obj.ty, &prop) != Some(true) {
            return Some((self.error_expr(span), false));
        }
        let mut stmts = vec![];
        self.push_scope();
        let recv = self.receiver_once(obj, object, later, &mut stmts);
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
        let part_ref = synth(
            ast::ExprKind::Ident(ast::Ident {
                name: PART.into(),
                span: pspan,
            }),
            pspan,
        );
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

    /// `(depth, object, prop)`: the outermost part `object.prop` strictly inside `target` that
    /// is a field read through an interface whose value a write in place would not reach.
    fn iface_part_of<'e>(
        &mut self,
        target: &'e ast::Expr,
    ) -> Option<(usize, &'e ast::Expr, ast::Ident)> {
        let mut cur = inner(target)?;
        let mut depth = 1;
        loop {
            if let ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } = &bare(cur).kind
            {
                if peekable(object) && self.copied_iface_part(object, prop) {
                    return Some((depth, object, prop.clone()));
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
        let ty = self.cx.ty.opt_payload(f.ty).unwrap_or(f.ty);
        match self.cx.ty.kind(ty) {
            TyKind::Tuple(_) => true,
            TyKind::Adt(d, _) => {
                let d = *d;
                self.cx
                    .adt(d)
                    .is_some_and(|a| a.kind != hir::AdtKind::Class)
            }
            _ => false,
        }
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
