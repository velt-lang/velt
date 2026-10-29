//! Which std functions the cache serves, and what each one's code may inline.
//!
//! Served: every function defined in std (generic, caller-tracking and frozen instances too)
//! except
//! - small ones ([`SMALL`]), which stay where callers can inline them;
//! - renamed duplicates (`$dup`), which are not the same function in every program;
//! - those whose inlinable set ([`inlinable`]) names a static with relocations or one that is
//!   written: vtables, type descriptors and module state are compared or written by address,
//!   and a copy in another object would be another address;
//! - those whose inlinable set calls through a function pointer: in the program `const_fields`
//!   specializes them for the closure their caller passes and inlines it (`Array.map`,
//!   `Map.upsert` ran 3-6% slower compiled on their own).

use std::collections::{BTreeMap, HashSet};

use velt_vir::vir::{self, Callee, Const, Linkage, Operand, Rvalue, Stmt, Terminator};

/// Functions of fewer statements (terminators included) than this stay in the program, where
/// they can be inlined (velt_opt's `SMALL_COST` is 40).
pub(super) const SMALL: usize = 40;

/// Whether `f` is small enough to stay where its callers can inline it.
pub(super) fn small(f: &vir::Function) -> bool {
    size(f) < SMALL
}

/// `f`'s statements, terminators included.
pub(super) fn size(f: &vir::Function) -> usize {
    f.blocks.iter().map(|b| b.stmts.len() + 1).sum()
}

fn is_std(f: &vir::Function) -> bool {
    velt_vir::mangle::demangle(&f.symbol).starts_with("std/")
}

/// The functions of `program` the cache serves.
pub(super) fn candidates(program: &vir::Program) -> Vec<usize> {
    (0..program.funcs.len())
        .filter(|&i| reason(program, i).is_none())
        .collect()
}

/// Why std function `i` stays in the program; `None` when the cache serves it.
fn reason(program: &vir::Program, i: usize) -> Option<&'static str> {
    let f = &program.funcs[i];
    if !is_std(f) || f.linkage != Linkage::Internal || f.blocks.is_empty() {
        return Some("not internal");
    }
    if small(f) {
        return Some("small");
    }
    if f.symbol.contains("$dup") {
        return Some("$dup");
    }
    let set = inlinable(program, i);
    if !set
        .iter()
        .all(|&j| plain_statics(program, &program.funcs[j]))
    {
        return Some("statics with identity");
    }
    if set
        .iter()
        .any(|&j| calls_through_pointer(&program.funcs[j]))
    {
        return Some("callbacks");
    }
    None
}

/// The std code of `program` by why it stays in the program (statements), and how much the
/// cache serves (`VELT_STD_OBJECT_STATS`).
pub(super) fn reasons(program: &vir::Program) -> BTreeMap<&'static str, usize> {
    let mut by = BTreeMap::new();
    for (i, f) in program.funcs.iter().enumerate() {
        if is_std(f) {
            *by.entry(reason(program, i).unwrap_or("served"))
                .or_default() += size(f);
        }
    }
    by
}

/// Function `i` and every small function it reaches through small ones: what its code compiled
/// on its own may inline. In a deterministic order, `i` first.
pub(super) fn inlinable(program: &vir::Program, i: usize) -> Vec<usize> {
    let mut seen = HashSet::from([i]);
    let mut order = vec![i];
    let mut n = 0;
    while let Some(&j) = order.get(n) {
        n += 1;
        for c in referenced(&program.funcs[j]) {
            let f = &program.funcs[c];
            if f.linkage == Linkage::Internal && small(f) && seen.insert(c) {
                order.push(c);
            }
        }
    }
    order
}

/// The functions the code of `set` (an [`inlinable`] set) calls without compiling them: what its
/// object declares, in a deterministic order.
pub(super) fn declared(program: &vir::Program, set: &[usize]) -> Vec<usize> {
    let mut seen: HashSet<usize> = set.iter().copied().collect();
    let mut out = vec![];
    for &j in set {
        for c in referenced(&program.funcs[j]) {
            if seen.insert(c) {
                out.push(c);
            }
        }
    }
    out
}

/// Whether every static `f` names holds plain bytes: no relocations, never written.
fn plain_statics(program: &vir::Program, f: &vir::Function) -> bool {
    let mut plain = true;
    visit(f, &mut |op| {
        if let Operand::Const(Const::Static(s), _) = op {
            let s = &program.statics[s.0 as usize];
            plain &= s.relocs.is_empty() && s.mutable.is_none();
        }
    });
    plain
}

/// Whether `f` calls through a function pointer.
fn calls_through_pointer(f: &vir::Function) -> bool {
    (f.blocks.iter()).any(|b| {
        matches!(
            &b.term,
            Terminator::Call {
                callee: Callee::Ptr { .. },
                ..
            }
        )
    })
}

/// The functions `f` calls or takes the address of.
pub(super) fn referenced(f: &vir::Function) -> Vec<usize> {
    let mut out = vec![];
    visit(f, &mut |op| {
        if let Operand::Const(Const::Func(id), _) = op {
            out.push(id.0 as usize);
        }
    });
    for b in &f.blocks {
        if let Terminator::Call {
            callee: Callee::Func(id),
            ..
        } = &b.term
        {
            out.push(id.0 as usize);
        }
    }
    out
}

/// Every operand of `f`'s code.
fn visit(f: &vir::Function, each: &mut dyn FnMut(&Operand)) {
    for b in &f.blocks {
        for s in &b.stmts {
            match s {
                Stmt::Assign(_, rv) => match rv {
                    Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => each(a),
                    Rvalue::Binary(_, a, c) => {
                        each(a);
                        each(c);
                    }
                    Rvalue::Aggregate(_, ops) => ops.iter().for_each(&mut *each),
                    Rvalue::AddrOf(_) => {}
                },
                Stmt::MemCopy { dst, src, .. } => {
                    each(dst);
                    each(src);
                }
                Stmt::MemCopyDyn { dst, src, len, .. } => {
                    each(dst);
                    each(src);
                    each(len);
                }
                Stmt::MemSet { dst, byte, len } => {
                    each(dst);
                    each(byte);
                    each(len);
                }
                Stmt::Nop => {}
            }
        }
        match &b.term {
            Terminator::Branch { cond, .. } => each(cond),
            Terminator::Switch { value, .. } => each(value),
            Terminator::Return(op) => each(op),
            Terminator::Call { callee, args, .. } => {
                if let Callee::Ptr { target, .. } = callee {
                    each(target);
                }
                args.iter().for_each(&mut *each);
            }
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
    }
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
