//! One inline-or-heap decision for a string's length and bytes (#2506).
//!
//! `strByteLen` and `strByteAt` each select on the string's last word. Left to each read, the
//! data pointer is hoisted out of a loop to below the loop's `n != 0` test, too late for LLVM to
//! merge it with the length's decision, so `fnv1a64` decides twice. After `const n = strByteLen(s)`
//! lowering works out the pointer on the spot, and the reads that follow use it. Nothing changes
//! but the place of that computation, and only under these conditions:
//!
//! - The call is the whole initializer of a `let` in the block (or that, cast), so it runs before
//!   everything after it in the block, nested blocks included. A `strByteLen` under `&&`, `?:`
//!   or a `switch` arm doesn't run on every path. Entries go when their block ends.
//! - `s` is a local.
//! - The rest of the block is quiet (quiet.rs, strict): it runs no user code, calls no extern
//!   but `velt_simd_*` and assigns nothing that needs a drop. So nothing there can grow a
//!   builder, a `string` the `velt_rt_strbuf_push_*` externs write with no assignment, or replace
//!   the owner of a place `s` borrows: the pointer stays right for as long as it is used.
//! - The rest of the block reads a byte of `s`. Otherwise the pointer would be extra work where
//!   nothing removes it (a build without optimization), and other code stays as it was.
//! - Not in an async function or a generator: a temp doesn't survive a suspension. Nor in a
//!   generic instance: quiet.rs asks about functions that aren't. Nor in a constructor: `super()`
//!   looks quiet, and field initializers run at it.
//! - A `finally` body lowered on an early exit runs after the quiet code, unchecked: it sees no
//!   entry (drops.rs `finally_body`).

use velt_sema::hir::{self, ExprKind as K, Intrinsic as I, LocalId, StmtKind as S};

use super::{quiet::Quiet, FnLower};
use crate::vir::Operand;

impl FnLower<'_, '_> {
    /// The statements of `b`, its scope open.
    pub(super) fn block_stmts(&mut self, b: &hir::Block) {
        for (i, s) in b.stmts.iter().enumerate() {
            self.stmt(s);
            let rest = (&b.stmts[i + 1..], b.value.as_deref());
            self.note_str_data(s, rest);
        }
    }

    /// After the statement `s`, with `rest` left of its block: see the module docs.
    pub(super) fn note_str_data(
        &mut self,
        s: &hir::Stmt,
        rest: (&[hir::Stmt], Option<&hir::Expr>),
    ) {
        let S::Let { init: Some(e), .. } = &s.kind else {
            return;
        };
        let e = match &e.kind {
            K::Cast(x) => x,
            _ => e,
        };
        let K::Call {
            callee: hir::Callee::Intrinsic(I::StrByteLen),
            args,
        } = &e.kind
        else {
            return;
        };
        let [hir::Expr {
            kind: K::Local(l, _),
            ..
        }] = args.as_slice()
        else {
            return;
        };
        let l = *l;
        if self.asyncx.is_some()
            || self.ctor_self.is_some()
            || !self.targs.is_empty()
            || self.dead()
            || !(rest.0.iter().any(|s| stmt_reads(s, l))
                || rest.1.is_some_and(|e| expr_reads(e, l)))
            || !self.cx.quiet_rest(rest.0, rest.1, Quiet::Strict)
        {
            return;
        }
        let p = self.local_place(l);
        let data = self.str_data(&p);
        self.str_datas.push((l, data, self.scopes.len()));
    }

    /// Where the bytes of the string `s` are, when `note_str_data` worked that out.
    pub(super) fn known_str_data(&self, s: &hir::Expr) -> Option<Operand> {
        let K::Local(l, _) = &s.kind else {
            return None;
        };
        let known = self.str_datas.iter().rev().find(|d| d.0 == *l)?;
        Some(known.1.clone())
    }
}

/// Does `b`, quiet, read a byte of `l`?
fn block_reads(b: &hir::Block, l: LocalId) -> bool {
    b.stmts.iter().any(|s| stmt_reads(s, l)) || b.value.as_deref().is_some_and(|e| expr_reads(e, l))
}

/// Every statement quiet code has.
fn stmt_reads(s: &hir::Stmt, l: LocalId) -> bool {
    match &s.kind {
        S::Let { init, .. } => init.as_ref().is_some_and(|e| expr_reads(e, l)),
        S::Expr(e) => expr_reads(e, l),
        S::Return(e) => e.as_ref().is_some_and(|e| expr_reads(e, l)),
        S::If { cond, then, els } => {
            expr_reads(cond, l)
                || block_reads(then, l)
                || els.as_ref().is_some_and(|b| block_reads(b, l))
        }
        S::While {
            cond, body, step, ..
        } => {
            expr_reads(cond, l)
                || block_reads(body, l)
                || step.as_ref().is_some_and(|e| expr_reads(e, l))
        }
        S::Block(b) => block_reads(b, l),
        _ => false,
    }
}

fn expr_reads(e: &hir::Expr, l: LocalId) -> bool {
    if let K::Call {
        callee: hir::Callee::Intrinsic(I::StrByteAt),
        args,
    } = &e.kind
    {
        if matches!(args.first(), Some(hir::Expr { kind: K::Local(x, _), .. }) if *x == l) {
            return true;
        }
    }
    if let K::Block(b) = &e.kind {
        return block_reads(b, l);
    }
    let mut found = false;
    super::frame_envs::children(e, &mut |x| found = found || expr_reads(x, l));
    found
}
