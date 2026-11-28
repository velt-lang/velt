//! Which conversions of a dynamic value to an object type lowering may make member by member
//! (velt_vir `any_object_fill.rs`): an object type with optional or literal-typed fields, or
//! fields an object literal's own field types may differ from (`any`, `unknown` and `object`,
//! options and unions: `{ reason: 1 }` held as `unknown`, then converted to an interface with
//! `reason: unknown`, #2554; `never`, which no value is, #3121), and a function with properties
//! (`#call` first), built from the
//! members of a value of another type, which lowering reads through `any`.

use velt_common::Span;

use crate::body::places::set_place_mode;
use crate::body::FnCx;
use crate::hir::{self, Intrinsic, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// Note that `exp`, the target of a conversion of a dynamic value, may be built from the
    /// members of a value of another type: lowering then has the member slots.
    pub(super) fn note_object_fill(&mut self, exp: TyId) {
        let TyKind::Adt(d, args) = self.cx.ty.kind(exp).clone() else {
            return;
        };
        let fields = self.cx.adt(d).map(|a| a.fields.clone()).unwrap_or_default();
        let fill = fields.iter().any(|f| {
            // No value is a `never` (#3121): an object never has the type exactly. At its type
            // arguments: `Box<never>` of `interface Box<T> { v: T }`.
            let never = self.cx.subst(f.ty, &args) == self.cx.ty.never;
            never
                || f.optional
                || f.name == "#call"
                || self.cx.lit_value(f.declared).is_some()
                || self.cx.dynamic(f.ty).is_some()
                || self.cx.ty.opt_payload(f.ty).is_some()
                || self.cx.union_def(f.ty).is_some()
        });
        if fill {
            self.cx.note_dyn_members();
        }
    }
}

impl FnCx<'_, '_> {
    /// `obj.name` of a member of type `never` (#3121): an object converted from `any` has one
    /// with no value in it (velt_vir `any_object_fill.rs`), and no local may hold a `never`, so
    /// the read stops with a `TypeError` once `obj` is evaluated, as a failed cast does.
    pub(super) fn never_field_read(
        &mut self,
        mut obj: hir::Expr,
        name: &str,
        span: Span,
    ) -> hir::Expr {
        set_place_mode(&mut obj, UseMode::Borrow);
        let on = self.cx.display(obj.ty);
        let msg = format!("TypeError: `{name}` of a `{on}` is `never`: no value is one to read");
        let m = self.str_lit(&msg, span);
        let never = self.cx.ty.never;
        let panic = self.intrinsic(Intrinsic::Panic, vec![m], never, span);
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(obj),
                span,
            }],
            value: Some(Box::new(panic)),
            span,
        };
        self.mk(hir::ExprKind::Block(block), never, span)
    }
}
