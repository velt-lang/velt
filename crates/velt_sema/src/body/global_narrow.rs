//! Narrowing of module variables (#1280, #1168): a module-level `let` (or a module `const` kept
//! in the module state) that a condition proves non-null (`if (handler) handler();`,
//! `if (!ctx) return; ctx.m()`) reads as its payload, as TypeScript narrows it.
//!
//! Any call may run code that reassigns a module variable (a function of its module, a
//! callback, a getter, another task across an `await`), and TypeScript keeps the narrowing
//! anyway. So a narrowed module variable gets a *token*, a hidden unit local of the frame whose
//! facts live in the scopes like a local's, and each narrowed read checks the value where it
//! happens, as `x!` does (in JavaScript's order: `B.n = clear()` writes the object `B` held
//! before the call): when something set the variable to `null` in between, the read stops with
//! a `TypeError` where JavaScript would go on with `null`; it never reads `null` as an object.
//! Field paths of the variable are checked again by lowering (`field_narrow`). Assigning the variable in the function drops the narrowing (and narrows it to what
//! was assigned), and so does a loop that assigns it, from the loop on. Closures and nested
//! functions start without the facts of the function they are made in.
//!
//! The token is also the root of narrowed field paths of the variable (`field_narrow`):
//! `if (R.t !== null) R.t.name` on a module `const R`.

use velt_common::Span;
use velt_syntax::ast;

use super::{FnCx, LocalKind, Want};
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as H, LocalId, TyId};

/// A module variable some condition of this function tested: its global, its name, the token.
#[derive(Clone)]
pub(crate) struct GlobalToken {
    def: DefId,
    name: String,
    token: LocalId,
}

impl FnCx<'_, '_> {
    /// The global `e` names when it is a module variable kept in the module state (a module
    /// `let`, or a `const` evaluated once) and no local of this or an enclosing function
    /// shadows it. `(x = v)` names `x`, as for locals (`named_local`). A constant evaluated
    /// at each use is not: a field path of it would be a field of a temporary.
    pub(crate) fn module_var(&mut self, e: &ast::Expr) -> Option<DefId> {
        let id = match &e.kind {
            ast::ExprKind::Ident(i) => i,
            ast::ExprKind::Paren(inner) => return self.module_var(inner),
            ast::ExprKind::Assign {
                op: None, target, ..
            } => return self.module_var(target),
            _ => return None,
        };
        if self.is_local_name(&id.name) {
            return None;
        }
        let Some(Item::Def(d)) = self.cx.lookup_item_at(self.module, &id.name, id.span) else {
            return None;
        };
        if !matches!(self.cx.info[d.0 as usize], DefInfo::Global(_)) {
            return None;
        }
        crate::body::driver::ensure_global(self.cx, d);
        match &self.cx.info[d.0 as usize] {
            DefInfo::Global(g) if g.stored => Some(d),
            _ => None,
        }
    }

    /// The token of module variable `e` ([`Self::module_var`]), created when missing.
    pub(crate) fn global_token(&mut self, e: &ast::Expr) -> Option<LocalId> {
        let d = self.module_var(e)?;
        if let Some(t) = self.f.global_tokens.iter().find(|t| t.def == d) {
            return Some(t.token);
        }
        let name = self.cx.global(d)?.name.clone();
        let unit = self.cx.ty.unit;
        let token = self.new_local(
            "<narrowed module variable>",
            unit,
            false,
            e.span,
            LocalKind::Temp,
        );
        self.f.global_tokens.push(GlobalToken {
            def: d,
            name,
            token,
        });
        Some(token)
    }

    /// The token of nullable module variable `e`, for a null or truthiness test.
    pub(crate) fn nullable_global(&mut self, e: &ast::Expr) -> Option<LocalId> {
        let d = self.module_var(e)?;
        let ty = self.cx.global(d)?.ty;
        self.cx.ty.opt_payload(ty)?;
        self.global_token(e)
    }

    /// The token of module variable `e` if a test in this function made one.
    pub(crate) fn existing_global_token(&mut self, e: &ast::Expr) -> Option<LocalId> {
        if self.f.global_tokens.is_empty() {
            return None;
        }
        let d = self.module_var(e)?;
        let found = self.f.global_tokens.iter().find(|t| t.def == d);
        found.map(|t| t.token)
    }

    /// The tokens of the module variables named `name` (code that assigns `name`; a local of
    /// that name shadowing them only drops more).
    pub(crate) fn global_tokens_named(&self, name: &str) -> Vec<LocalId> {
        let tokens = self.f.global_tokens.iter();
        tokens.filter(|t| t.name == name).map(|t| t.token).collect()
    }

    /// The type of the module variable whose token is `t`.
    pub(crate) fn token_global_ty(&self, t: LocalId) -> Option<TyId> {
        let mut tokens = self.f.global_tokens.iter();
        let d = tokens.find(|g| g.token == t).map(|g| g.def)?;
        self.cx.global(d).map(|g| g.ty)
    }

    /// A read of module variable `d` (named at `span`) that a null test narrowed: its payload,
    /// checked where it is read. `None` when it is not narrowed here, or when the read does not
    /// need the payload (`needs_payload`).
    pub(crate) fn narrowed_global(
        &mut self,
        d: DefId,
        exp: Option<TyId>,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if self.f.global_tokens.is_empty() {
            return None;
        }
        let raw = self.cx.global(d)?.ty;
        if !self.needs_payload(span, exp, want, false, raw) {
            return None;
        }
        let found = self.f.global_tokens.iter().find(|t| t.def == d);
        let t = found.map(|t| t.token)?;
        if !self.is_narrowed(t) {
            return None;
        }
        let g = self.cx.global(d)?;
        let ty = g.ty;
        // The name as written (`cur`, not the qualified `state::cur`).
        let name = g.name.rsplit("::").next().unwrap_or(&g.name).to_string();
        self.present_payload(ty)?;
        let what = self.cx.ty.nothing_of(ty).map_or("null", |n| match n {
            hir::Nothing::Both => "null or undefined",
            n => n.word(),
        });
        let msg =
            format!("TypeError: `{name}` was checked, but it was set to {what} before this read");
        // Taken by value: another reference to what the module state keeps (`global_read`).
        let read = match want {
            Want::Move => self.global_read(d, Want::Move, span),
            _ => self.mk(H::Global(d), ty, span),
        };
        // As `x!` reads it (`non_null`), which keeps the value alive for its use when a call
        // clears the variable meanwhile (#1161).
        Some(self.checked_present(read, &msg, None, false, span))
    }
}
