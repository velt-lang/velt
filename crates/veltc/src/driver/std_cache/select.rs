//! Which std functions the cache serves, and what each one's code may inline.
//!
//! Served: every function defined in std (generic, caller-tracking and frozen instances too)
//! except
//! - in a release build, small ones ([`SMALL`]) and those std marks `inline()`, which stay where
//!   callers can inline them; a debug build inlines neither, so it links them too, and keeps only
//!   the helpers its optimizer level inlines ([`Rule::Debug`]);
//! - renamed duplicates (`$dup`), which are not the same function in every program;
//! - those whose inlinable set ([`inlinable`]) reaches a static with identity (relocations, or
//!   written: vtables, type descriptors, module state) that can't be named: such statics are
//!   named and used by address from the program (`statics`), unless another static has the same
//!   contents;
//! - in a release build, those whose inlinable set calls through a function pointer: in the
//!   program `const_fields` specializes them for the closure their caller passes and inlines it
//!   (`Array.map`, `Map.upsert` ran 3-6% slower compiled on their own). A debug build specializes
//!   nothing.

use std::collections::{BTreeMap, HashSet};

use velt_vir::vir::{self, Callee, Const, Inline, Linkage, Operand, Rvalue, Stmt, Terminator};

use super::statics::{self, Names};

/// Functions of fewer statements (terminators included) than this stay in the program, where
/// they can be inlined (velt_opt's `SMALL_COST` is 40).
pub(super) const SMALL: usize = 40;

/// Which std functions stay in the program for its callers' sake.
#[derive(Clone, Copy)]
pub(super) enum Rule<'a> {
    /// Those its optimizer may inline ([`inlines`]).
    Release,
    /// A debug build: those `velt_opt::debug_inlined` marks. Calls of the others stay calls in
    /// its code, so what serves them can only make it faster. `thread_data`: the program's
    /// thread-local values are data that an object can share
    /// (`velt_codegen_cl::has_thread_local_data`); else code reaching one stays in the program too.
    Debug {
        helpers: &'a [bool],
        thread_data: bool,
    },
}

/// Whether the optimizer inlines `f` into a function compiled with it: it is small, or std marks
/// it `inline()`. Its body goes into what calls it, so it is in their keys.
pub(super) fn inlines(f: &vir::Function) -> bool {
    size(f) < SMALL || f.inline == Inline::Always
}

/// `f`'s statements, terminators included.
pub(super) fn size(f: &vir::Function) -> usize {
    f.blocks.iter().map(|b| b.stmts.len() + 1).sum()
}

/// Where the source map holds what std's generators wrote for a program (veltc's
/// `generate::GENERATED_DIR`, then the generator's std path).
const GENERATED_STD: &str = "<generated>/std/";

/// Whether `f` is std's code: defined in std, or written for this program by one of std's
/// generators (`JSON.parse<T>`'s reader of `T`). Generated code is served like a generic
/// instance: its key is its code, so a program that generates the same code links it.
fn is_std(f: &vir::Function) -> bool {
    let name = velt_vir::mangle::demangle(&f.symbol);
    velt_common::std_modules::is_std(&name) || name.starts_with(GENERATED_STD)
}

/// The functions of `program` the cache serves (`names`: its statics' names).
pub(super) fn candidates(program: &vir::Program, names: &Names, rule: Rule) -> Vec<usize> {
    (0..program.funcs.len())
        .filter(|&i| reason(program, names, rule, i).is_none())
        .collect()
}

/// Why std function `i` stays in the program; `None` when the cache serves it.
fn reason(program: &vir::Program, names: &Names, rule: Rule, i: usize) -> Option<&'static str> {
    let f = &program.funcs[i];
    if !is_std(f) || f.linkage != Linkage::Internal || f.blocks.is_empty() {
        return Some("not internal");
    }
    match rule {
        Rule::Release if inlines(f) => return Some("small"),
        Rule::Debug { helpers, .. } if helpers.get(i) == Some(&true) => return Some("small"),
        _ => {}
    }
    if f.symbol.contains("$dup") {
        return Some("$dup");
    }
    let set = inlinable(program, i);
    if matches!(
        rule,
        Rule::Debug {
            thread_data: false,
            ..
        }
    ) && set.iter().any(|&j| reaches_thread_local(program, j))
    {
        return Some("thread-local values");
    }
    if !statics::nameable(names, &statics::reached(program, &set)) {
        return Some("statics with identity");
    }
    if matches!(rule, Rule::Release)
        && set
            .iter()
            .any(|&j| calls_through_pointer(&program.funcs[j]))
    {
        return Some("callbacks");
    }
    None
}

/// The std code of `program` by why it stays in the program (statements), and how much the
/// cache serves (`VELT_STD_OBJECT_STATS`).
pub(super) fn reasons(
    program: &vir::Program,
    names: &Names,
    rule: Rule,
) -> BTreeMap<&'static str, usize> {
    let mut by = BTreeMap::new();
    for (i, f) in program.funcs.iter().enumerate() {
        if is_std(f) {
            *by.entry(reason(program, names, rule, i).unwrap_or("served"))
                .or_default() += size(f);
        }
    }
    by
}

/// Function `i` and every function the optimizer inlines ([`inlines`]) that it reaches through
/// such functions: what its code compiled on its own may inline. In a deterministic order, `i`
/// first.
pub(super) fn inlinable(program: &vir::Program, i: usize) -> Vec<usize> {
    let mut seen = HashSet::from([i]);
    let mut order = vec![i];
    let mut n = 0;
    while let Some(&j) = order.get(n) {
        n += 1;
        for c in referenced(&program.funcs[j]) {
            let f = &program.funcs[c];
            if f.linkage == Linkage::Internal && inlines(f) && seen.insert(c) {
                order.push(c);
            }
        }
    }
    order
}

/// The functions the code of `set` (an [`inlinable`] set) calls without compiling them, and those
/// the statics it reaches name (it may load and call them): what its object declares, in a
/// deterministic order.
pub(super) fn declared(program: &vir::Program, set: &[usize]) -> Vec<usize> {
    let mut seen: HashSet<usize> = set.iter().copied().collect();
    let mut out = vec![];
    let named = statics::functions(program, &statics::reached(program, set));
    let called = set.iter().flat_map(|&j| referenced(&program.funcs[j]));
    for c in called.chain(named) {
        if seen.insert(c) {
            out.push(c);
        }
    }
    out
}

/// Whether function `j` reads or writes a thread-local value.
fn reaches_thread_local(program: &vir::Program, j: usize) -> bool {
    (program.funcs[j].blocks.iter()).any(|b| match &b.term {
        Terminator::Call {
            callee: Callee::Extern(e),
            ..
        } => velt_vir::tls::TlsCall::of_symbol(&program.ext(*e).symbol).is_some(),
        _ => false,
    })
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
pub(super) fn visit(f: &vir::Function, each: &mut dyn FnMut(&Operand)) {
    for b in &f.blocks {
        for s in &b.stmts {
            match s {
                Stmt::Assign(_, rv) => match rv {
                    Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => each(a),
                    Rvalue::Binary(_, a, c) => {
                        each(a);
                        each(c);
                    }
                    Rvalue::Aggregate(_, ops) | Rvalue::Intrinsic(_, ops) => {
                        ops.iter().for_each(&mut *each)
                    }
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
