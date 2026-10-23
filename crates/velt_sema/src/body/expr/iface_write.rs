//! `x.f = v` where `x` is an interface value or a bounded generic and `f` a field of the
//! interface: a call of the field's setter slot (`crate::iface_writes`). Compound forms
//! (`x.f += v`, `x.f++`, `x.f ??= v`) read through the getter slot and write through the setter
//! slot, like accessors ([`accessor_rmw`](super::accessor_rmw)).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::Callable;
use crate::body::FnCx;
use crate::defs::{Bound, ParamSig};
use crate::hir::{self, Callee, DefId, ExprKind as H, PassMode, TyId, TyKind};

/// A field of an interface, as a value of type `T extends I` or `I` sees it.
pub(crate) struct IfaceField {
    pub iface: DefId,
    pub args: Vec<TyId>,
    /// The field's index in the interface.
    pub index: usize,
    /// Its type with the interface's arguments substituted.
    pub ty: TyId,
    pub readonly: bool,
    /// The receiver is a type parameter (a `ParamMethod` call), else an interface value.
    pub on_param: bool,
}

impl FnCx<'_, '_> {
    /// The interface field `name` that values of type `t` read through a getter slot.
    pub(crate) fn iface_field_of(&mut self, t: TyId, name: &str) -> Option<IfaceField> {
        let (bounds, on_param) = match self.cx.ty.kind(t) {
            TyKind::Dyn(d, args) => (
                vec![Bound {
                    iface: *d,
                    args: args.clone(),
                }],
                false,
            ),
            TyKind::Param(n) => (
                self.bounds.get(*n as usize).cloned().unwrap_or_default(),
                true,
            ),
            // `T & { … }`: a member of an object part (`crate::intersection_members`).
            TyKind::Op(..) => {
                let f = self.cx.part_field(t, name)?;
                return Some(IfaceField {
                    iface: f.iface,
                    args: f.args,
                    index: f.index,
                    ty: f.ty,
                    readonly: f.readonly,
                    on_param: true,
                });
            }
            _ => return None,
        };
        for b in bounds {
            let Some(i) = self.cx.iface(b.iface) else {
                continue;
            };
            let Some(index) = i.fields.iter().position(|f| f.name == name) else {
                continue;
            };
            let (ty, readonly) = (i.fields[index].ty, i.fields[index].readonly);
            let ty = self.cx.subst(ty, &b.args);
            return Some(IfaceField {
                iface: b.iface,
                args: b.args,
                index,
                ty,
                readonly,
                on_param,
            });
        }
        None
    }

    /// Is `x.prop` (`x` of type `t`) a field written through an interface? A writable one gives
    /// the interface its setter slots; a `readonly` one is reported (`Some(false)`).
    pub(super) fn iface_write_target(&mut self, t: TyId, prop: &ast::Ident) -> Option<bool> {
        let f = self.iface_field_of(t, &prop.name)?;
        if f.readonly {
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "cannot assign to `{}`: it is a readonly field",
                        self.cx.shown_name(&prop.name)
                    ),
                    prop.span,
                )
                .with_note(format!("`{}` declares it `readonly`", self.cx.display(t))),
            );
            return Some(false);
        }
        // Generic code may write the field of any type it is instantiated with.
        self.cx
            .object_copies
            .writes
            .push(crate::object_copies::Write {
                ty: None,
                name: prop.name.clone(),
                span: prop.span,
                dynamic: false,
            });
        if f.args.iter().any(|a| self.cx.dynamic(*a).is_some()) {
            // Only a value at a dynamic argument may be an adapter (`iface_adapt`).
            self.cx.note_field_written(f.iface, f.index, prop.span);
        }
        Some(self.cx.mark_iface_written(f.iface, prop.span))
    }

    /// A part of interface field `f` (`prop`) is changed and written back through its setter
    /// slot (`iface_nested.rs`), which a `readonly` field has for this only. False: reported.
    pub(super) fn iface_part_target(&mut self, f: &IfaceField, prop: &ast::Ident) -> bool {
        self.cx
            .object_copies
            .writes
            .push(crate::object_copies::Write {
                ty: None,
                name: prop.name.clone(),
                span: prop.span,
                dynamic: false,
            });
        self.cx.mark_part_written(f.iface, f.index, prop.span)
    }

    /// `recv.prop = arg` through the setter slot of interface field `f`.
    pub(super) fn iface_setter_call(
        &mut self,
        recv: hir::Expr,
        f: IfaceField,
        arg: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let Some(slot) = self.cx.setter_slot(f.iface, f.index) else {
            self.check_args_loose(std::slice::from_ref(arg));
            return self.error_expr(span);
        };
        let mode = if self.cx.is_copy(f.ty) {
            PassMode::Copy
        } else {
            PassMode::Owned
        };
        let c = Callable {
            what: "the field's setter".into(),
            params: vec![ParamSig {
                name: "v".into(),
                span,
                ty: f.ty,
                mode,
                default: None,
            }],
            ret: self.cx.ty.unit,
            slot_names: vec![],
            bounds: vec![],
            constraints: vec![],
            js_numbers: false,
            positions: false,
            js_api: false,
            rest: false,
            defaults: vec![],
        };
        let ck = self.check_call(&c, vec![], std::slice::from_ref(arg), None, span);
        let recv = self.receiver(recv, None, PassMode::BorrowMut, false);
        let mut args = vec![recv];
        args.extend(ck.args);
        let callee = if f.on_param {
            Callee::ParamMethod {
                iface: f.iface,
                iface_args: f.args,
                slot,
                method_type_args: vec![],
            }
        } else {
            Callee::Dyn { slot }
        };
        let unit = self.cx.ty.unit;
        self.mk(H::Call { callee, args }, unit, span)
    }
}
