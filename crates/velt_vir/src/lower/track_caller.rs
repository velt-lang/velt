//! Caller-tracking standard-library functions (Rust's `#[track_caller]`, implicitly): a
//! synchronous, capture-free function defined in a standard-library file whose body calls
//! `panic` directly (or calls another such function) reports the location of its call site
//! when it panics. Every direct call site gets its own instance (`Work::Tracked`), so the
//! location is a compile-time constant and the calling convention is unchanged; indirect uses
//! (function values, vtables) keep the ordinary instance.

use velt_sema::hir::{self, DefId, Intrinsic};

use super::Cx;

impl Cx<'_> {
    /// Whether calls of `def` report their call site when it panics (memoized).
    pub(super) fn tracks_caller(&mut self, def: DefId) -> bool {
        if let Some(&t) = self.tracked.get(&def) {
            return t;
        }
        // Provisionally false: breaks recursion cycles.
        self.tracked.insert(def, false);
        let hir_prog = self.hir;
        let tracked = match (hir_prog.def(def), self.locs.as_ref()) {
            (hir::Def::Fn(f), Some(m))
                if m.is_std(f.span)
                    && !f.is_async
                    && !f.is_generator
                    && f.captures.is_empty()
                    && !untracked(&f.name) =>
            {
                let mut callees = vec![];
                let direct = block_panics(&f.body.block, &mut callees);
                direct || callees.into_iter().any(|d| self.tracks_caller(d))
            }
            _ => false,
        };
        self.tracked.insert(def, tracked);
        tracked
    }
}

/// The prelude's text of an `any` (velt_sema `any_text.rs`): `__anyString` and
/// `__anyConcatText` hold a dispatch over every class of the program, and the others are what
/// they call. Too large to copy per call site, each keeps one instance and takes the user's call
/// site as its last argument (`at`, a `"path:line:col"` constant per site, like the hidden
/// location argument of Rust's `#[track_caller]`), which its stops write into their messages:
/// their panics get no suffix of their own ([`Cx::reports_own_site`]).
const UNTRACKED: [&str; 12] = [
    "__anyString",
    "__anyConcatText",
    "__anyConcatPlain",
    "__anyText",
    "__anyPrimText",
    "__anyPrimTry",
    "__anyTextUnsupported",
    "__anyJoin",
    "__anyJoinSeen",
    "__anyToPrimitive",
    "__anyToPrimitivePlain",
    "__anyPrimKeep",
];

/// Is the function named `name` (its last path segment) one of [`UNTRACKED`]?
fn untracked(name: &str) -> bool {
    let last = name.rsplit(['.', ':', '/']).next().unwrap_or(name);
    UNTRACKED.contains(&last)
}

impl Cx<'_> {
    /// Whether `def` is a standard-library function of [`UNTRACKED`], whose stops report the
    /// call site passed to it (its panics get no ` at …` suffix).
    pub(super) fn reports_own_site(&self, def: DefId) -> bool {
        match (self.hir.def(def), self.locs.as_ref()) {
            (hir::Def::Fn(f), Some(m)) => m.is_std(f.span) && untracked(&f.name),
            _ => false,
        }
    }
}

/// Whether `b` calls `panic` directly; collects the direct callees for the transitive check.
fn block_panics(b: &hir::Block, callees: &mut Vec<DefId>) -> bool {
    let mut found = false;
    for s in &b.stmts {
        found |= stmt_panics(s, callees);
    }
    if let Some(v) = &b.value {
        found |= expr_panics(v, callees);
    }
    found
}

fn stmt_panics(s: &hir::Stmt, callees: &mut Vec<DefId>) -> bool {
    use hir::StmtKind as S;
    match &s.kind {
        S::Let { init: Some(e), .. } | S::Expr(e) | S::Return(Some(e)) => expr_panics(e, callees),
        S::LetPat { init, .. } => expr_panics(init, callees),
        S::If { cond, then, els } => {
            let mut f = expr_panics(cond, callees) | block_panics(then, callees);
            if let Some(b) = els {
                f |= block_panics(b, callees);
            }
            f
        }
        S::While {
            cond, body, step, ..
        } => {
            let step = step.as_ref().is_some_and(|e| expr_panics(e, callees));
            expr_panics(cond, callees) | block_panics(body, callees) | step
        }
        S::ForOf { iter, body, .. } => expr_panics(iter, callees) | block_panics(body, callees),
        S::Try {
            body,
            catch,
            finally,
        } => {
            let mut f = block_panics(body, callees);
            if let Some((_, h)) = catch {
                f |= block_panics(h, callees);
            }
            if let Some(b) = finally {
                f |= block_panics(b, callees);
            }
            f
        }
        S::Block(b) => block_panics(b, callees),
        S::Let { init: None, .. } | S::Return(None) | S::Break(_) | S::Continue(_) => false,
    }
}

fn exprs_panic(es: &[hir::Expr], callees: &mut Vec<DefId>) -> bool {
    es.iter().fold(false, |f, e| f | expr_panics(e, callees))
}

fn expr_panics(e: &hir::Expr, callees: &mut Vec<DefId>) -> bool {
    use hir::ExprKind as K;
    match &e.kind {
        K::Call { callee, args } => {
            let own = match callee {
                hir::Callee::Intrinsic(Intrinsic::Panic) => true,
                hir::Callee::Indirect(x) => expr_panics(x, callees),
                hir::Callee::Def(d, _) => {
                    callees.push(*d);
                    false
                }
                _ => false,
            };
            own | exprs_panic(args, callees)
        }
        K::Unary { expr: x, .. }
        | K::Cast(x)
        | K::Await(x)
        | K::WrapSome(x)
        | K::UnwrapSome(x, _)
        | K::UnwrapVariant { expr: x, .. }
        | K::Upcast(x)
        | K::Downcast(x)
        | K::ToDyn { expr: x, .. }
        | K::Throw(x)
        | K::Field { base: x, .. } => expr_panics(x, callees),
        K::Binary { lhs, rhs, .. }
        | K::Logical { lhs, rhs, .. }
        | K::Assign {
            place: lhs,
            value: rhs,
        }
        | K::CompoundAssign {
            place: lhs,
            value: rhs,
            ..
        }
        | K::Index {
            base: lhs,
            index: rhs,
            ..
        } => expr_panics(lhs, callees) | expr_panics(rhs, callees),
        K::If { cond, then, els } => {
            expr_panics(cond, callees) | expr_panics(then, callees) | expr_panics(els, callees)
        }
        K::Block(b) => block_panics(b, callees),
        K::AdtLit { fields: es, .. }
        | K::Variant { args: es, .. }
        | K::ArrayLit(es)
        | K::Tuple(es)
        | K::New { args: es, .. } => exprs_panic(es, callees),
        K::Match { scrutinee, arms } => {
            let mut f = expr_panics(scrutinee, callees);
            for arm in arms {
                f |= arm.guard.as_ref().is_some_and(|g| expr_panics(g, callees));
                f |= expr_panics(&arm.body, callees);
            }
            f
        }
        K::Lit(_) | K::Local(..) | K::Global(_) | K::FnRef(..) | K::Closure(_) => false,
    }
}
