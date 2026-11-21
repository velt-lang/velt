//! `delete o.f` (or `delete o["f"]`) of an optional field `f?: T` of an object type or an
//! interface, which TypeScript allows (TS2790 refuses it for a required field). Velt keeps such
//! a field as its value or nothing, an absent field and one holding `undefined` being one state
//! (`crate::absent_fields`), or keeps "absent" apart with a presence flag
//! (`hir::FieldDef::presence`). Deleting it assigns it `Intrinsic::FieldAbsent`: its value is
//! `undefined` and its flag clear (velt_vir `assign_expr`), and the `delete` is `true`, as in
//! JavaScript. `"f" in o` is then `false`, and `Object.keys`, `JSON.stringify` and `console.log`
//! leave it out, as Node does. `crate::absent_fields` doesn't count that assignment as a store of
//! `undefined`.
//!
//! A class's optional field is an own property from construction (TypeScript-compiled classes
//! define it), which Velt has no way to remove: it stays refused.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::accessor_rmw::block;
use super::record_call::RecordKey;
use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyKind};
use crate::symbols::Presence;

impl FnCx<'_, '_> {
    /// `delete operand` of an optional field (`None`: `operand` is not one).
    pub(super) fn delete_optional(
        &mut self,
        operand: &ast::Expr,
        object: &ast::Expr,
        key: &RecordKey,
        span: Span,
    ) -> Option<hir::Expr> {
        let name = match key {
            RecordKey::Name(p) => p.name.clone(),
            RecordKey::Index(i) => match &i.kind {
                ast::ExprKind::Lit(ast::Lit::Str(s)) => s.clone(),
                _ => return None,
            },
            RecordKey::Computed(_) => return None,
        };
        let t = self.trial_type(object)?;
        let presence = self.cx.declared_presence(t, &name);
        if presence.is_none() || presence == Some(Presence::No) {
            return None;
        }
        let (optional, readonly) = self.field_flags(t, &name).unwrap_or((false, false));
        let refuse = |cx: &mut Self, d: Diagnostic| {
            cx.cx.error(d);
            Some(cx.error_expr(span))
        };
        if readonly {
            return refuse(self, Diagnostic::error(
                "the operand of a `delete` operator cannot be a read-only property (TypeScript's TS2704)",
                span,
            ));
        }
        match presence {
            Some(Presence::Optional(_)) => {}
            Some(Presence::Yes) if optional && self.cx.class_of(t).is_some() => {
                let d = Diagnostic::error(format!("cannot `delete` a field of class `{}`", self.cx.display(t)), span)
                    .with_note("a class's field is an own property of every instance from construction, which Velt has no way to remove: assign it `undefined` instead");
                return refuse(self, d);
            }
            _ => {
                return refuse(
                    self,
                    Diagnostic::error(
                        "the operand of a `delete` operator must be optional (TypeScript's TS2790)",
                        span,
                    ),
                );
            }
        }
        let undefined = synth(ast::ExprKind::Lit(ast::Lit::Undefined), span);
        let assign = synth(
            ast::ExprKind::Assign {
                op: None,
                target: Box::new(operand.clone()),
                value: Box::new(undefined),
            },
            span,
        );
        let unit = self.cx.ty.unit;
        let bool_ = self.cx.ty.bool_;
        let mut assigned = self.expr(&assign, Some(unit), Want::Move);
        // Absent, not a present `undefined`: a presence flag is cleared (velt_vir `assign_expr`).
        // The value is the `undefined` made above, also where a dynamic field (`a?: any`)
        // converted it.
        if let H::Assign { value, .. } = &mut assigned.kind {
            value.kind = H::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::FieldAbsent),
                args: vec![],
            };
        }
        let stmt = hir::Stmt {
            kind: hir::StmtKind::Expr(assigned),
            span,
        };
        let yes = self.mk(H::Lit(hir::Lit::Bool(true)), bool_, span);
        Some(self.mk(H::Block(block(vec![stmt], Some(yes), span)), bool_, span))
    }

    /// Whether field `name` of a value of type `t` (an object type, a class and its bases, an
    /// interface) is optional and whether it is `readonly`.
    fn field_flags(&self, t: hir::TyId, name: &str) -> Option<(bool, bool)> {
        let flags = |f: &crate::defs::FieldInfo| (f.optional, f.readonly);
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => {
                let mut d = *d;
                for _ in 0..32 {
                    let a = self.cx.adt(d)?;
                    if let Some(f) = a.fields.iter().find(|f| f.name == name) {
                        return Some(flags(f));
                    }
                    d = a.base.and_then(|b| self.cx.class_of(b))?.0;
                }
                None
            }
            TyKind::Dyn(d, _) => self
                .cx
                .iface(*d)?
                .fields
                .iter()
                .find(|f| f.name == name)
                .map(flags),
            _ => None,
        }
    }
}
