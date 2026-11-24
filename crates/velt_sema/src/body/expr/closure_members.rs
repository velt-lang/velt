//! An arrow checked against a union of function types (`(() => void) | (() => Promise<void>)`):
//! the member it is typed by, the first it type-checks against (closure.rs `closure`), each try
//! rolled back. A `void` member doesn't fit an expression body that gives a promise when another
//! member returns one: it would drop the promise (#3644).

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// The first of `members` that arrow `e` type-checks against (each try is rolled back), or
    /// the first one, whose errors the real check then reports.
    pub(super) fn member_by_trial(
        &mut self,
        e: &ast::Expr,
        members: &[TyId],
        escaping: bool,
    ) -> TyId {
        // Remembered per arrow, members and the types of the enclosing locals it names, as
        // narrowed here: an arrow nested in one being tried is tried once, not once per try of
        // each enclosing arrow (exponential in the nesting), unless a local it uses has another
        // type in this try (`(x) => { const y = x; show(() => y); }`, `x` a number, then a string). Locals
        // it doesn't name (the enclosing arrows' other locals) don't change how it checks.
        let key = (e.span, members.to_vec(), self.named_local_types(e));
        if let Some(&m) = self.member_choices.get(&key) {
            return m;
        }
        let m = self.first_fitting(e, members, escaping);
        self.member_choices.insert(key, m);
        m
    }

    fn first_fitting(&mut self, e: &ast::Expr, members: &[TyId], escaping: bool) -> TyId {
        // A member returning a promise, for an expression body that gives one: a `void` member
        // would drop it (`pull: (c) => p.then(...)`, whose rejection must reach the stream).
        let promising = members.iter().any(|&m| self.returns_promise(m));
        for &m in members {
            let mark = crate::body::recheck::Mark::here(self.cx);
            let frames = self.trial_frames();
            let diags = self.cx.diags.len();
            let saved = self.dropped_promise.replace((e.span, false));
            let h = self.closure(e, Some(m), escaping);
            let dropped = matches!(self.dropped_promise, Some((_, true)));
            self.dropped_promise = saved;
            let ok = self.cx.diags.len() == diags && h.ty == m && !(dropped && promising);
            mark.rollback(self.cx);
            self.restore_trial_frames(frames);
            if ok {
                return m;
            }
        }
        members[0]
    }

    /// Does the function type `m` return a promise (or a promise or nothing)?
    fn returns_promise(&self, m: TyId) -> bool {
        match self.cx.ty.kind(m) {
            hir::TyKind::FnPtr { ret, .. } => self.holds_promise(*ret),
            _ => false,
        }
    }

    /// Is `t` a promise type, or an optional one (`Promise<void> | undefined`)?
    pub(super) fn holds_promise(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            hir::TyKind::Promise(..) => true,
            hir::TyKind::Option(x, _) => matches!(self.cx.ty.kind(*x), hir::TyKind::Promise(..)),
            _ => false,
        }
    }
}
