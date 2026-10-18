//! `const`s that closures in their own initializer use (#750, docs/reference/functions.md
//! "A closure using the constant it initializes"): `const app: App = { use(p) { p(app); return
//! app; } }`, `const effectFn: Subscriber = () => { … effectFn.flags … }`.
//!
//! The closure is created before the constant has its value, so it captures the variable, not
//! a value: the constant is declared before its initializer (with the annotated type, or the
//! type a trial check of the initializer gives), left unassigned, and assigned once the
//! initializer has run. A closure that keeps the capture assigns it after the capture, so the
//! variable lives in a shared cell (`crate::ownership::cells`), which the closure reads when it
//! runs, as in JavaScript. A hidden flag (`<ready x>`, also captured) is set after the
//! assignment; each statement of a closure reading the constant first checks it and panics with
//! JavaScript's message (`ReferenceError: Cannot access 'x' before initialization`) while the
//! initializer is still running (the temporal dead zone). An expression-bodied arrow checks at
//! its start. Closures created once the constant is assigned capture its value as usual.
//!
//! An object that holds a closure capturing the object is a reference cycle, which counting
//! does not free: one object per such constant created stays allocated (documented in
//! docs/reference/functions.md).

use std::collections::HashMap;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, LocalId, StmtKind as S, TyId, UseMode};

/// A frame's part in late `const`s.
#[derive(Clone, Debug, Default)]
pub(crate) struct LateFrame {
    /// The late `const`s this frame declares whose initializer is running, with their flags.
    pub pending: HashMap<LocalId, LocalId>,
    /// This frame's captures of a late `const` made while its initializer ran, with this
    /// frame's capture of the flag.
    pub caps: HashMap<LocalId, LocalId>,
    /// The flags the statement being checked reads a late `const` through (checked before it),
    /// with the constant's name.
    pub checks: Vec<(LocalId, String)>,
}

/// The name of the flag of late `const` `name` (no source can spell it).
fn flag_name(name: &str) -> String {
    format!("<ready {name}>")
}

/// Does a closure (arrow, function expression, object literal method) in `e` name `name`?
pub(super) fn closure_uses(e: &ast::Expr, name: &str) -> bool {
    use crate::nested_closures::scan::{scan_expr, scan_fn};
    let found = match &e.kind {
        ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_) => {
            let s = scan_expr(e);
            s.refs.contains(name) && !s.binds.contains(name)
        }
        ast::ExprKind::Object(props) | ast::ExprKind::StructLit { props, .. } => {
            props.iter().any(|p| match p {
                ast::ObjectProp::Method(d) => {
                    let s = scan_fn(d);
                    s.refs.contains(name) && !s.binds.contains(name)
                }
                _ => false,
            })
        }
        _ => false,
    };
    if found {
        return true;
    }
    if matches!(
        e.kind,
        ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_)
    ) {
        return false;
    }
    let mut any = false;
    crate::ast_walk::children(e, &mut |c| any = any || closure_uses(c, name));
    any
}

impl FnCx<'_, '_> {
    /// `const name[: ann] = init;` whose initializer has a closure using `name`: declared
    /// before the initializer and assigned after it (module docs). False when it is no such
    /// constant.
    pub(super) fn late_const_decl(
        &mut self,
        v: &ast::VarDecl,
        name: &ast::Ident,
        ann: Option<TyId>,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) -> bool {
        let Some(e) = v.init.as_ref() else {
            return false;
        };
        if v.kind != ast::VarKind::Const || !closure_uses(e, &name.name) {
            return false;
        }
        let ty = match ann {
            Some(t) => t,
            None => self.late_trial_ty(name, e),
        };
        let bool_ = self.cx.ty.bool_;
        let flag = self.new_local(
            &flag_name(&name.name),
            bool_,
            true,
            name.span,
            LocalKind::Let,
        );
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(flag_name(&name.name), flag);
        let no = self.mk(H::Lit(hir::Lit::Bool(false)), bool_, name.span);
        Self::push(
            out,
            S::Let {
                local: flag,
                init: Some(no),
            },
            span,
        );
        let local = self.declare_local(name, ty, LocalKind::Const);
        self.cx.late_consts.insert(name.span);
        self.f.late.pending.insert(local, flag);
        Self::push(out, S::Let { local, init: None }, span);
        let h = self.expr_coerce(e, ty, Want::Move);
        self.f.late.pending.remove(&local);
        let unit = self.cx.ty.unit;
        for (l, t, value) in [
            (local, ty, h),
            (
                flag,
                bool_,
                self.mk(H::Lit(hir::Lit::Bool(true)), bool_, name.span),
            ),
        ] {
            let place = self.mk(H::Local(l, UseMode::BorrowMut), t, name.span);
            let assign = H::Assign {
                place: Box::new(place),
                value: Box::new(value),
            };
            let assign = self.mk(assign, unit, span);
            Self::push(out, S::Expr(assign), span);
        }
        true
    }

    /// The type of late `const` `name`'s initializer `e`, checked as a trial (rolled back)
    /// with `name` of the error type; reported when it depends on `name` itself.
    fn late_trial_ty(&mut self, name: &ast::Ident, e: &ast::Expr) -> TyId {
        let mark = super::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let err = self.cx.ty.error;
        self.push_scope();
        self.declare_local(name, err, LocalKind::Const);
        let h = self.expr(e, None, Want::Move);
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        if self.cx.ty.has_error(h.ty) {
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "type annotation needed for `{}`: a closure in its initializer uses it",
                        name.name
                    ),
                    name.span,
                )
                .with_note(format!("write its type, e.g. `const {}: T = …`", name.name)),
            );
        }
        h.ty
    }

    /// Local `inner` of this frame, captured from frame `j`'s `outer` (or the current frame's
    /// own local when `j` is `None`), was just looked up as `name`: when it is a late `const`
    /// whose initializer is running, its flag is captured too, and this statement checks it.
    pub(super) fn late_lookup(
        &mut self,
        j: Option<usize>,
        outer: LocalId,
        inner: LocalId,
        name: &str,
        span: Span,
    ) {
        if let Some(&flag) = self.f.late.caps.get(&inner) {
            self.late_check(flag, name);
            return;
        }
        let Some(j) = j else { return };
        let fr = &self.outer[j];
        let late = fr.late.pending.contains_key(&outer) || fr.late.caps.contains_key(&outer);
        if !late {
            return;
        }
        let fname = flag_name(name);
        let Some(flag) = self.lookup_local(&fname, span) else {
            return;
        };
        // Every closure frame in between captured both: their reads check too.
        for f in &mut self.outer[j + 1..] {
            let lookup = |n: &str| f.scopes.iter().rev().find_map(|s| s.names.get(n).copied());
            if let (Some(x), Some(r)) = (lookup(name), lookup(&fname)) {
                f.late.caps.insert(x, r);
            }
        }
        self.f.late.caps.insert(inner, flag);
        self.late_check(flag, name);
    }

    fn late_check(&mut self, flag: LocalId, name: &str) {
        if !self.f.late.checks.iter().any(|(f, _)| *f == flag) {
            self.f.late.checks.push((flag, name.to_string()));
        }
    }

    /// The checks of `checks` as statements (module docs).
    pub(crate) fn late_check_stmts(
        &self,
        checks: Vec<(LocalId, String)>,
        span: Span,
    ) -> Vec<hir::Stmt> {
        let bool_ = self.cx.ty.bool_;
        let never = self.cx.ty.never;
        checks
            .into_iter()
            .map(|(flag, name)| {
                let r = self.mk(H::Local(flag, UseMode::Copy), bool_, span);
                let cond = self.mk(
                    H::Unary {
                        op: hir::UnOp::Not,
                        expr: Box::new(r),
                    },
                    bool_,
                    span,
                );
                let msg = format!("ReferenceError: Cannot access '{name}' before initialization");
                let msg = self.str_lit(&msg, span);
                let panic = self.intrinsic(hir::Intrinsic::Panic, vec![msg], never, span);
                let then = hir::Block {
                    stmts: vec![hir::Stmt {
                        kind: S::Expr(panic),
                        span,
                    }],
                    value: None,
                    span,
                };
                hir::Stmt {
                    kind: S::If {
                        cond,
                        then,
                        els: None,
                    },
                    span,
                }
            })
            .collect()
    }
}
