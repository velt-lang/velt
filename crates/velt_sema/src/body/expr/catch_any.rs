//! The `catch` of Node's `TypeError` for a call through `any` that holds no function (#1422).
//! velt_vir throws that error to the innermost `catch` of the same function when the `catch`'s
//! type takes a `TypeError` (velt_vir any_not_fn.rs). Typing never grows for it, with one
//! exception that changes no program that compiles: a `catch` whose `try` throws nothing (its
//! variable is `never`) but calls through `any` gets the type `TypeError` when its body does not
//! use the variable (a `never` variable read ends the path there, which a `TypeError` would
//! not: unused, both are the same), or else when its body checks the same either way.
//!
//! A `never` variable's first use ends the path through the `catch`; a `TypeError` one lets it
//! go on. So the body is checked as a trial both ways ([`FnCx::handler_trial`]), and takes the
//! `TypeError` only when
//!
//! - it checks without a diagnostic and throws the same (a `throw e` would make the function
//!   throw a `TypeError`; `throw new Error(…)` throws what it threw before; the members of a
//!   `TypeError` are standard library calls that throw nothing), leaves the same narrowing,
//!   and, when the `try` body ends every path, ends (`flow::block_diverges`) the same way;
//! - the code that now runs after the variable's use is checked against nothing a `never`
//!   variable left unchecked ([`fits_handler`]): of the variables declared outside the `catch`
//!   it uses only `const`s and parameters (always assigned) and no promise (a promise moved to
//!   a spawned call can't be used again), it assigns none of them, creates no closure, declares
//!   no `let` without a value or `using` one it moves, has no loop, `break` or `continue`, and
//!   returns no value made from its own variables while the function's result type is
//!   inferred; and
//! - the path through it reaches the code after the `try` with the facts the path through the
//!   `try` body brings: the `try` body leaves everything as it was ([`plain_try`]: it assigns
//!   and moves no variable declared outside it, narrows nothing, always reaches its end), or
//!   the `catch` never reaches its end (`return`, `throw`). A throw or `return` out of it leaves
//!   with the state its start has, which the moves check already passes on to the enclosing
//!   `catch` and `finally` blocks for every `catch` (`moves::tries`).
//!
//! Anything else keeps `never`, and the call stops the program as before.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::locals::NarrowState;
use crate::body::{FnCx, LocalKind};
use crate::defs::ThrowSrc;
use crate::hir::{self, TyId, UseMode};
use crate::visit::VisitMut;

/// Whether a `try` body calls through `any`, and if so whether it leaves the facts the code
/// after the `try` is checked against as they were (module docs).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnyTry {
    No,
    Plain,
    Changes,
}

/// What a trial check of a `catch` body found ([`FnCx::handler_trial`]).
struct Trial {
    /// No diagnostic, and [`fits_handler`].
    fits: bool,
    /// What the body throws.
    throws: Vec<ThrowSrc>,
    narrow: NarrowState,
    diverges: bool,
}

impl FnCx<'_, '_> {
    /// A call through `any` was made here (dynamic_member.rs): counted in user modules only,
    /// where velt_vir throws the `TypeError`.
    pub(crate) fn note_any_call(&mut self) {
        if !self.cx.scopes[self.module].is_std {
            self.f.any_calls += 1;
        }
    }

    /// The type of a `catch`'s variable: `ety` from what its `try` throws (`srcs`), or
    /// `TypeError` for a `never` one whose `try` calls through `any` (`calls_any`, with whether
    /// the `try` body ends every path: `flow::block_diverges`), under the module docs' rule.
    pub(crate) fn not_fn_catch_type(
        &mut self,
        ety: TyId,
        (calls_any, body_diverges): (AnyTry, bool),
        pat: Option<&ast::Pattern>,
        blk: &ast::Block,
        srcs: &[ThrowSrc],
        span: Span,
    ) -> TyId {
        if ety != self.cx.ty.never || calls_any == AnyTry::No {
            return ety;
        }
        let name = match pat.map(|p| &p.kind) {
            None => None,
            Some(ast::PatternKind::Ident(n)) => Some(n.name.as_str()),
            Some(_) => return ety,
        };
        let Some(d) = self.cx.prelude_adt("TypeError") else {
            return ety;
        };
        let te = self.cx.ty.intern(crate::hir::TyKind::Adt(d, vec![]));
        if name.is_none_or(|n| !used(blk, n)) {
            return te;
        }
        let key = (blk.span, self.named_local_types(&wrap(blk)));
        let fits = match self.catch_choices.get(&key) {
            Some(&fits) => fits,
            None => {
                let fits =
                    self.takes_type_error(srcs, pat, blk, te, (calls_any, body_diverges), span);
                self.catch_choices.insert(key, fits);
                fits
            }
        };
        if fits {
            te
        } else {
            ety
        }
    }

    /// Does the `catch` body `blk` check the same with a `TypeError` variable (`te`) as with a
    /// `never` one (module docs)?
    fn takes_type_error(
        &mut self,
        srcs: &[ThrowSrc],
        pat: Option<&ast::Pattern>,
        blk: &ast::Block,
        te: TyId,
        (calls_any, body_diverges): (AnyTry, bool),
        span: Span,
    ) -> bool {
        let t = self.handler_trial(srcs, pat, blk, te, span);
        if !t.fits || !(calls_any == AnyTry::Plain || t.diverges) {
            return false;
        }
        let never = self.cx.ty.never;
        let n = self.handler_trial(srcs, pat, blk, never, span);
        // The `try` statement ends every path when its body and its `catch` do.
        self.same_throws(&t.throws, &n.throws)
            && t.narrow == n.narrow
            && (!body_diverges || t.diverges == n.diverges)
    }

    /// The `catch` body `blk` checked with its variable of type `ety`, rolled back.
    fn handler_trial(
        &mut self,
        srcs: &[ThrowSrc],
        pat: Option<&ast::Pattern>,
        blk: &ast::Block,
        ety: TyId,
        span: Span,
    ) -> Trial {
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let temps = self.call_temps.len();
        let first = self.f.locals.len();
        let thrown = self.thrown().len();
        let (_, body) = self.handler(srcs.to_vec(), pat, blk, ety, span);
        let inferred = self.f.ret.is_none();
        let t = Trial {
            fits: self.cx.diags.len() == diags
                && fits_handler(&body, first, &self.f.kinds, inferred, |t| {
                    self.cx.holds_promise(t)
                }),
            throws: self.thrown()[thrown..].to_vec(),
            narrow: self.narrow_state(),
            diverges: crate::flow::block_diverges(&body, &self.cx.ty),
        };
        mark.rollback(self.cx);
        self.cx.diags.truncate(diags);
        self.call_temps.truncate(temps);
        self.restore_trial_frames(frames);
        t
    }

    /// Does a `catch` body that throws `te` with a `TypeError` variable throw what it throws
    /// with a `never` one (`nv`)? A `TypeError`'s members are calls of the standard library's
    /// that throw nothing.
    fn same_throws(&mut self, te: &[ThrowSrc], nv: &[ThrowSrc]) -> bool {
        let mut rest: Vec<String> = nv.iter().map(|s| format!("{s:?}")).collect();
        for s in te {
            let k = format!("{s:?}");
            if let Some(i) = rest.iter().position(|r| *r == k) {
                rest.swap_remove(i);
                continue;
            }
            let std_call = matches!(s, ThrowSrc::Call(g, ..)
                if self.cx.try_fn(*g).is_some_and(|f| self.cx.scopes[f.module].is_std));
            let never = self.cx.ty.never;
            let quiet = std_call
                && crate::throws::srcs_now(self.cx, std::slice::from_ref(s))
                    .is_none_or(|t| t == never);
            if !quiet {
                return false;
            }
        }
        rest.is_empty()
    }

    /// Where a throw here is recorded (`FnCx::throw_src`).
    fn thrown(&self) -> &[ThrowSrc] {
        match self.f.tries.last() {
            Some(t) => t,
            None => &self.f.uncaught,
        }
    }
}

/// Is the `catch` body `b` (checked with a `TypeError` variable, the first of its locals
/// `first`) one whose every run after its variable's use is checked against what it was before
/// (module docs)? `kinds` are the function's local kinds; `inferred`: its result type is
/// inferred from its `return`s.
fn fits_handler(
    b: &hir::Block,
    first: usize,
    kinds: &[LocalKind],
    inferred: bool,
    mut promise: impl FnMut(TyId) -> bool,
) -> bool {
    use hir::ExprKind as H;
    use hir::StmtKind as S;
    struct Fits<'k, C> {
        first: usize,
        kinds: &'k [LocalKind],
        inferred: bool,
        promise: C,
        /// The next local occurrence is an allowed read of an outer one.
        read: bool,
        ok: bool,
    }
    impl<C: FnMut(TyId) -> bool> Fits<'_, C> {
        fn own(&self, l: hir::LocalId) -> bool {
            l.0 as usize >= self.first
        }
        /// Does `e` mention one of the handler's own locals?
        fn mentions_own(&self, e: &hir::Expr) -> bool {
            struct Own(usize, bool);
            impl VisitMut for Own {
                fn local(&mut self, l: &mut hir::LocalId) {
                    self.1 |= l.0 as usize >= self.0;
                }
            }
            let mut o = Own(self.first, false);
            crate::visit::expr(&mut e.clone(), &mut o);
            o.1
        }
    }
    impl<C: FnMut(TyId) -> bool> VisitMut for Fits<'_, C> {
        fn stmt(&mut self, s: &mut hir::Stmt) {
            match &s.kind {
                S::Let { init: None, .. }
                | S::While { .. }
                | S::ForOf { .. }
                | S::Break(_)
                | S::Continue(_) => self.ok = false,
                S::Return(Some(v)) if self.inferred && self.mentions_own(v) => self.ok = false,
                _ => {}
            }
        }
        fn expr(&mut self, e: &mut hir::Expr) {
            match &e.kind {
                // A `const` or parameter is initialized wherever it can be read; a value moved
                // for good (a promise, `moves`) or a `using` one would make a later use an error.
                H::Local(l, mode) => {
                    let k = self.kinds.get(l.0 as usize);
                    let fixed = matches!(k, Some(LocalKind::Const | LocalKind::Param));
                    let moves = *mode == UseMode::Move
                        && (matches!(k, Some(LocalKind::Using)) || (self.promise)(e.ty));
                    let outer = !self.own(*l);
                    self.read = outer && fixed && !(self.promise)(e.ty);
                    self.ok &= !moves && (!outer || self.read);
                }
                H::Assign { place, .. } | H::CompoundAssign { place, .. } => {
                    if root(place).is_none_or(|l| !self.own(l)) {
                        self.ok = false;
                    }
                }
                H::Closure(_) => self.ok = false,
                _ => {}
            }
        }
        fn local(&mut self, l: &mut hir::LocalId) {
            if !self.own(*l) && !std::mem::take(&mut self.read) {
                self.ok = false;
            }
        }
    }
    let mut v = Fits {
        first,
        kinds,
        inferred,
        promise: &mut promise,
        read: false,
        ok: true,
    };
    crate::visit::block(&mut b.clone(), &mut v);
    v.ok
}

/// The variable a place `e` (`x`, `x.f`, `x[i]`) is part of.
fn root(e: &hir::Expr) -> Option<hir::LocalId> {
    use hir::ExprKind as H;
    match &e.kind {
        H::Local(l, _) => Some(*l),
        H::Field { base, .. }
        | H::Index { base, .. }
        | H::UnwrapSome(base, _)
        | H::UnwrapVariant { expr: base, .. }
        | H::Downcast(base) => root(base),
        _ => None,
    }
}

/// Does the `try` body `b` (whose locals from `outer` on are its own) leave every fact the code
/// after the `try` is checked against as it was before it: no assignment to, or move of, a
/// variable declared outside it, no closure (which may assign one), and no `return`, `throw`,
/// `break`, `continue` or expression of type `never` (it always reaches its end)?
pub(crate) fn plain_try(b: &hir::Block, outer: usize, never: TyId) -> bool {
    struct Plain {
        outer: usize,
        never: TyId,
        ok: bool,
    }
    impl VisitMut for Plain {
        fn stmt(&mut self, s: &mut hir::Stmt) {
            use hir::StmtKind as S;
            if matches!(s.kind, S::Return(_) | S::Break(_) | S::Continue(_)) {
                self.ok = false;
            }
        }
        fn expr(&mut self, e: &mut hir::Expr) {
            use hir::ExprKind as H;
            let outer = |l: hir::LocalId| (l.0 as usize) < self.outer;
            if e.ty == self.never {
                self.ok = false;
            }
            match &e.kind {
                H::Assign { place, .. } | H::CompoundAssign { place, .. } => {
                    if root(place).is_none_or(outer) {
                        self.ok = false;
                    }
                }
                H::Local(l, UseMode::Move) if outer(*l) => self.ok = false,
                H::Closure(_) | H::Throw(_) => self.ok = false,
                _ => {}
            }
        }
    }
    let mut v = Plain {
        outer,
        never,
        ok: true,
    };
    crate::visit::block(&mut b.clone(), &mut v);
    v.ok
}

/// Does the name `name` occur in `blk` (nested functions included)?
fn used(blk: &ast::Block, name: &str) -> bool {
    struct Uses<'n>(&'n str, bool);
    impl<'a> velt_syntax::visit::Visit<'a> for Uses<'_> {
        fn expr(&mut self, e: &'a ast::Expr) {
            match &e.kind {
                ast::ExprKind::Ident(i) if i.name == self.0 => self.1 = true,
                // `{ e }`: the visitor skips shorthand properties.
                ast::ExprKind::Object(props) | ast::ExprKind::StructLit { props, .. }
                    if shorthand_uses(props, self.0) > 0 =>
                {
                    self.1 = true
                }
                _ => {}
            }
        }
    }
    let mut u = Uses(name, false);
    velt_syntax::visit::walk_expr(&wrap(blk), &mut u);
    u.1
}

/// How many shorthand properties of an object literal (`{ e }`) name `name`.
fn shorthand_uses(props: &[ast::ObjectProp], name: &str) -> usize {
    props
        .iter()
        .filter(|p| matches!(p, ast::ObjectProp::Shorthand(i) if i.name == name))
        .count()
}

/// `blk` as an arrow's body, for the expression visitor.
fn wrap(blk: &ast::Block) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Arrow {
            type_params: vec![],
            params: vec![],
            ret: None,
            throws: None,
            body: ast::ArrowBody::Block(blk.clone()),
            is_async: false,
        },
        span: blk.span,
    }
}
