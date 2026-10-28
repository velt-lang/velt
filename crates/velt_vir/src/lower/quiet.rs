//! Quiet functions: a direct call to one needs no stabilized borrows (stabilize.rs), because
//! the callee cannot reach the counted objects a borrow goes through, nor replace or free
//! anything. That is what a runtime extern guarantees too (`Stabilize::No` in call.rs).
//!
//! A function is quiet when its body runs no user code and drops nothing it did not make: it
//! is not generic, a closure, async or a generator (a method is asked about at a direct call,
//! where its own body runs: `this.db.header(page)` borrows `this.db`); its statements are `let`s,
//! assignments, `if`, `while`, `return`, `break` and `continue`, and expressions; it only
//! assigns values that need no drop, to locals or through its params (elements and fields);
//! and it only calls quiet functions, `velt_simd_*` externs and intrinsics that run no user
//! code and are handed no place to modify or move. Anything else (`new`, closures, `await`,
//! `try`, `throw`, `for…of`, patterns, indirect, virtual and interface calls, module state)
//! makes it not quiet, and so does a field read or write on a type that may hold a typed proxy
//! (proxy_typed.rs): it may run a `get` or `set` trap, which is user code. Std's byte readers
//! and writers (`loadF64(this.buf.bytes, o)` in a typed array's element access) are quiet, so
//! the access counts no reference per element.
//!
//! A function is *quiet for borrows* ([`Quiet::Borrows`]) when the same holds while it can
//! still read through what it was lent, which allows two more things, so that a reader such as
//! a record decoder (`decodeHeader(this.db.data, …, this.types, this.offsets)`) borrows its
//! arguments for free too:
//! - `push` of a value that needs no drop onto an array it reaches from a param or local: it
//!   runs no user code and drops nothing, but may move the array's buffer, so a caller still
//!   shares a borrow of an element (an argument held in an array's buffer, `in_array_buffer`);
//! - a call of a function whose result is `never` (`corrupt("record header size")`) given only
//!   literals and quiet values that need no drop (so it is lent none of the borrows): whatever
//!   it runs (an error's constructor) runs after the last read through the borrows, since
//!   nothing after the call runs but the way out of every quiet caller (quiet functions have
//!   no `try`), which drops nothing that needs a drop. Not quiet in the strict sense: that call
//!   may write memory (view_unswitch_plain.rs relies on a quiet call not writing any).

use velt_sema::hir::{self, DefId, ExprKind as H, StmtKind as S};

use super::Cx;

/// How quiet a function is asked to be (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Quiet {
    /// Runs no user code and writes only through its params and locals.
    Strict,
    /// Runs no user code that could replace or free what it was lent while it reads through it.
    Borrows,
}

impl Cx<'_> {
    /// Is `def` a quiet function (module docs)?
    pub(super) fn quiet(&mut self, def: DefId) -> bool {
        self.quiet_as(def, Quiet::Strict)
    }

    /// Is `def` quiet for borrows (module docs)? A strictly quiet function is.
    pub(super) fn quiet_for_borrows(&mut self, def: DefId) -> bool {
        self.quiet_as(def, Quiet::Borrows)
    }

    /// Memoized; a function reached again while it is being decided (recursion) counts as not
    /// quiet.
    fn quiet_as(&mut self, def: DefId, level: Quiet) -> bool {
        if let Some(&q) = self.quiet.get(&(def, level)) {
            return q;
        }
        self.quiet.insert((def, level), false);
        let hir::Def::Fn(f) = self.hir.def(def) else {
            return false;
        };
        // A method is asked about only at a direct call (`call_def`), where its body is the one
        // that runs; `this` is a param like any other.
        let q = f.generics == 0
            && !f.is_async
            && !f.is_generator
            && f.captures.is_empty()
            && self.quiet_block(&f.body.block, level);
        self.quiet.insert((def, level), q);
        q
    }

    fn quiet_block(&mut self, b: &hir::Block, level: Quiet) -> bool {
        b.stmts.iter().all(|s| self.quiet_stmt(s, level))
            && b.value.as_deref().is_none_or(|v| self.quiet_expr(v, level))
    }

    fn quiet_stmt(&mut self, s: &hir::Stmt, level: Quiet) -> bool {
        match &s.kind {
            S::Let { local: _, init } => init.as_ref().is_none_or(|e| self.quiet_value(e, level)),
            S::Expr(e) => self.quiet_expr(e, level),
            S::Return(e) => e.as_ref().is_none_or(|e| self.quiet_expr(e, level)),
            S::If { cond, then, els } => {
                self.quiet_expr(cond, level)
                    && self.quiet_block(then, level)
                    && els.as_ref().is_none_or(|b| self.quiet_block(b, level))
            }
            S::While {
                cond, body, step, ..
            } => {
                self.quiet_expr(cond, level)
                    && self.quiet_block(body, level)
                    && step.as_ref().is_none_or(|e| self.quiet_expr(e, level))
            }
            S::Break(_) | S::Continue(_) => true,
            S::Block(b) => self.quiet_block(b, level),
            S::LetPat { .. } | S::ForOf { .. } | S::Try { .. } => false,
        }
    }

    /// A quiet expression whose value needs no drop (stored in a local or a place).
    fn quiet_value(&mut self, e: &hir::Expr, level: Quiet) -> bool {
        !self.needs_drop(e.ty) && self.quiet_expr(e, level)
    }

    fn quiet_expr(&mut self, e: &hir::Expr, level: Quiet) -> bool {
        match &e.kind {
            H::Lit(_) | H::Local(..) => true,
            H::Unary { expr: x, .. } | H::Cast(x) => self.quiet_expr(x, level),
            H::Binary { lhs, rhs, .. } | H::Logical { lhs, rhs, .. } => {
                self.quiet_expr(lhs, level) && self.quiet_expr(rhs, level)
            }
            H::Assign { place, value } | H::CompoundAssign { place, value, .. } => {
                // The old value is dropped too: neither may need a drop.
                !self.needs_drop(place.ty)
                    && self.quiet_place(place, level)
                    && self.quiet_value(value, level)
            }
            H::If { cond, then, els } => {
                self.quiet_expr(cond, level)
                    && self.quiet_expr(then, level)
                    && self.quiet_expr(els, level)
            }
            H::Block(b) => self.quiet_block(b, level),
            H::Field { base, mode, .. } => {
                *mode != hir::UseMode::Move
                    && !self.needs_drop(e.ty)
                    && !self.proxiable(base.ty)
                    && self.quiet_expr(base, level)
            }
            H::Index { base, index, mode } => {
                *mode != hir::UseMode::Move
                    && !self.needs_drop(e.ty)
                    && self.quiet_expr(base, level)
                    && self.quiet_expr(index, level)
            }
            // A struct of plain values (`Varint { value, len }`): it lives in the function's
            // own memory.
            H::AdtLit {
                fields, converted, ..
            } => {
                *converted == hir::Conversion::No
                    && !self.needs_drop(e.ty)
                    && fields.iter().all(|f| self.quiet_value(f, level))
            }
            H::Call { callee, args } => {
                let callee_quiet = match callee {
                    hir::Callee::Def(def, targs) => match self.hir.def(*def) {
                        // Whatever a call that never returns runs, runs after the borrows'
                        // last use (module docs), if it is lent none of them.
                        hir::Def::Fn(f) if level == Quiet::Borrows && self.is_never(f.ret) => {
                            return args.iter().all(|a| {
                                matches!(a.kind, H::Lit(_)) || self.quiet_value(a, level)
                            });
                        }
                        hir::Def::Fn(_) => targs.is_empty() && self.quiet_as(*def, level),
                        hir::Def::ExternFn(x) => !x.is_async && x.symbol.starts_with("velt_simd_"),
                        _ => false,
                    },
                    // Onto an array the function reaches itself, a value that needs no drop.
                    hir::Callee::Intrinsic(hir::Intrinsic::ArrayPush)
                        if level == Quiet::Borrows =>
                    {
                        return match args.as_slice() {
                            [array, value] => {
                                self.quiet_place(array, level) && self.quiet_value(value, level)
                            }
                            _ => false,
                        };
                    }
                    hir::Callee::Intrinsic(i) => {
                        pure_intrinsic(*i) && !args.iter().any(velt_sema::effects::writes_place)
                    }
                    _ => false,
                };
                callee_quiet && args.iter().all(|a| self.quiet_expr(a, level))
            }
            _ => false,
        }
    }

    /// The target of an assignment: a local, or an element or field reached from one.
    fn quiet_place(&mut self, p: &hir::Expr, level: Quiet) -> bool {
        match &p.kind {
            H::Local(..) => true,
            H::Field { base, .. } => !self.proxiable(base.ty) && self.quiet_place(base, level),
            H::Index { base, index, .. } => {
                self.quiet_place(base, level) && self.quiet_expr(index, level)
            }
            _ => false,
        }
    }
}

/// Intrinsics that only read their arguments and compute: no user code (unlike `MutexWith`,
/// `JsonStringify`, `Print` or `ToString`, which call methods), no allocation, nothing dropped.
fn pure_intrinsic(i: hir::Intrinsic) -> bool {
    use hir::Intrinsic as I;
    matches!(
        i,
        I::ArrayLen
            | I::StrLen
            | I::StrByteLen
            | I::StrCharCodeAt
            | I::StrByteAt
            | I::StrLoadU8x16
            | I::StrLoadPartialU8x16
            | I::Sqrt
            | I::Floor
            | I::Ceil
            | I::Round
            | I::Trunc
            | I::FAbs
            | I::SimdShuffle(..)
    )
}
