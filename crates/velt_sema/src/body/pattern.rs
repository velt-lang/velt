//! Patterns of destructuring declarations and `for...of` bindings, and literal patterns (the
//! `case` values of `switch`, literal tests on unions).
//!
//! Binding modes: Copy payloads are `Copy`; otherwise a binding into a *temporary* value
//! moves (`Move`), a binding into a *place* borrows (`Borrow`, turned into `Move` by the
//! ownership pass if the binding is moved from — which then moves the destructured place).
//! `for...of` bindings always borrow. Negative integer literal patterns are encoded as the two's
//! complement bit pattern in the value's width (`-1i64` → `Lit::Int(0xffff_ffff_ffff_ffff)`).

use velt_common::Span;
use velt_syntax::ast;

use super::{FnCx, LocalKind};
use crate::hir::{self, PatKind as P, TyId, TyKind, UseMode};
use crate::types::{int_max, int_name};

#[derive(Clone, Copy, Debug)]
pub(crate) enum BindCtx {
    Let { mutable: bool, place: bool },
    Elem { mutable: bool },
}

impl FnCx<'_, '_> {
    pub fn pattern(&mut self, p: &ast::Pattern, ty: TyId, ctx: BindCtx) -> hir::Pat {
        if let Some(inner) = self.cx.ty.opt_payload(ty).filter(|_| matches_payload(p)) {
            // Destructuring a `T | null` value destructures its non-null payload.
            let sub = self.pattern(p, inner, ctx);
            return hir::Pat {
                kind: P::Some(Box::new(sub)),
                ty,
                span: p.span,
            };
        }
        let kind = self.pattern_kind(p, ty, ctx);
        hir::Pat {
            kind,
            ty,
            span: p.span,
        }
    }

    fn pattern_kind(&mut self, p: &ast::Pattern, ty: TyId, ctx: BindCtx) -> P {
        match &p.kind {
            ast::PatternKind::Wildcard => P::Wildcard,
            ast::PatternKind::Ident(name) => self.binding(name, ty, ctx),
            ast::PatternKind::Object { fields, rest } => {
                if let Some(r) = rest {
                    self.cx
                        .err("`...rest` in object patterns is not supported yet", r.span);
                }
                self.object_pattern(fields, ty, ctx)
            }
            ast::PatternKind::Array { elems, rest } => {
                self.array_pattern(elems, rest.as_ref(), ty, ctx, p.span)
            }
        }
    }

    pub(super) fn binding(&mut self, name: &ast::Ident, ty: TyId, ctx: BindCtx) -> P {
        let copy = self.cx.is_copy(ty);
        let (mode, kind, mutable) = match ctx {
            BindCtx::Let { mutable, place } => (
                if place {
                    UseMode::Borrow
                } else {
                    UseMode::Move
                },
                LocalKind::Bind,
                mutable,
            ),
            BindCtx::Elem { mutable } => (UseMode::Borrow, LocalKind::Elem, mutable && copy),
        };
        let mode = if copy { UseMode::Copy } else { mode };
        let l = self.declare_local_mut(name, ty, kind, mutable);
        P::Binding(l, mode)
    }

    fn object_pattern(
        &mut self,
        fields: &[(ast::Ident, ast::Pattern)],
        ty: TyId,
        ctx: BindCtx,
    ) -> P {
        let err = self.cx.ty.error;
        let mut out = vec![];
        for (name, sub) in fields {
            let found = self.field_of(ty, &name.name);
            let (idx, fty) = match found {
                Some(x) => {
                    self.check_field_private(ty, x.0, name);
                    if let Some((d, _)) = self.adt_of(ty) {
                        self.cx
                            .rec_ref(name.span, crate::ide::record::Target::Field(d, x.0));
                    }
                    x
                }
                None => {
                    if !self.cx.ty.is_bottom(ty) {
                        let tn = self.cx.display(ty);
                        self.cx.err(
                            format!("no field `{}` on type `{tn}`", name.name),
                            name.span,
                        );
                    }
                    (0, err)
                }
            };
            let sp = self.pattern(sub, fty, ctx);
            out.push((idx, sp));
        }
        P::Adt { fields: out }
    }

    fn array_pattern(
        &mut self,
        elems: &[ast::Pattern],
        rest: Option<&ast::Ident>,
        ty: TyId,
        ctx: BindCtx,
        span: Span,
    ) -> P {
        let err = self.cx.ty.error;
        match self.cx.ty.kind(ty).clone() {
            TyKind::Tuple(ts) => {
                if ts.len() != elems.len() || rest.is_some() {
                    let tn = self.cx.display(ty);
                    self.cx.err(
                        format!("pattern does not match the tuple type `{tn}`"),
                        span,
                    );
                }
                let pats = elems
                    .iter()
                    .enumerate()
                    .map(|(i, e)| self.pattern(e, ts.get(i).copied().unwrap_or(err), ctx))
                    .collect();
                P::Tuple(pats)
            }
            TyKind::Array(e) => {
                if !elems.is_empty() {
                    self.reject_promise_destructuring(e, span);
                }
                let pats = elems.iter().map(|p| self.pattern(p, e, ctx)).collect();
                let rest = rest.map(|r| {
                    if !self.cx.is_copy(e) && !self.cx.is_shared_value(e) {
                        self.cx.err(
                            "`...rest` needs an array of Copy or shared elements (not promises)",
                            r.span,
                        );
                    }
                    self.declare_local(r, ty, LocalKind::Bind)
                });
                P::Array { elems: pats, rest }
            }
            TyKind::Error => {
                for p in elems {
                    self.pattern(p, err, ctx);
                }
                P::Wildcard
            }
            _ => {
                let tn = self.cx.display(ty);
                self.cx.err(
                    format!("cannot destructure a value of type `{tn}` with `[...]`"),
                    span,
                );
                P::Wildcard
            }
        }
    }

    /// A literal pattern of type `ty` (negative ints as two's complement bits).
    pub(crate) fn pat_lit(&mut self, l: &ast::SignedLit, ty: TyId, span: Span) -> Option<hir::Lit> {
        let t = &self.cx.ty;
        let bottom = t.is_bottom(ty);
        let lit = match (&l.lit, t.kind(ty)) {
            (ast::Lit::Int { value, .. }, TyKind::Int(it)) => {
                let it = *it;
                if *value > int_max(it, l.negative)
                    || (l.negative && !it.is_signed() && *value != 0)
                {
                    self.cx
                        .err(format!("literal out of range for `{}`", int_name(it)), span);
                    return None;
                }
                let bits = it.bits();
                let mask = if bits == 128 {
                    u128::MAX
                } else {
                    (1u128 << bits) - 1
                };
                let v = if l.negative {
                    value.wrapping_neg() & mask
                } else {
                    *value
                };
                hir::Lit::Int(v)
            }
            (ast::Lit::Float { value, .. }, TyKind::Float(_)) => {
                hir::Lit::Float(if l.negative { -value } else { *value })
            }
            (ast::Lit::Int { value, .. }, TyKind::Float(_)) if !l.negative || *value > 0 => {
                let v = *value as f64;
                hir::Lit::Float(if l.negative { -v } else { v })
            }
            (ast::Lit::Str(s), TyKind::Str) if !l.negative => hir::Lit::Str(s.clone()),
            (ast::Lit::Bool(b), TyKind::Bool) if !l.negative => hir::Lit::Bool(*b),
            _ if bottom => return None,
            _ => {
                let tn = self.cx.display(ty);
                self.cx.err(
                    format!(
                        "mismatched types: this literal can never equal a value of type `{tn}`"
                    ),
                    span,
                );
                return None;
            }
        };
        Some(lit)
    }
}

/// A destructuring pattern (on `T | null` it destructures the payload).
fn matches_payload(p: &ast::Pattern) -> bool {
    matches!(
        p.kind,
        ast::PatternKind::Object { .. } | ast::PatternKind::Array { .. }
    )
}
