//! Constant aggregate fields: propagates fields of aggregate locals that always hold the same
//! constant (typically the code pointer of a closure `{ code, env }`) into the reads of those
//! fields — directly and through read-only pointers — and specializes callees that receive
//! such a pointer. Afterwards constfold turns the indirect closure calls into direct calls,
//! which the inliner can then inline.
//!
//! - `readonly`: which pointer params / locals are only read through (whole program).
//! - `fields`: per-function constant-field analysis and the read rewrite.
//! - `specialize`: cloning callees for known closure arguments.

mod fields;
mod readonly;
mod specialize;

use velt_vir::vir::{FuncId, Program};

pub(crate) use specialize::Specializations;

/// Run over the whole program; returns whether anything changed.
pub(crate) fn run(program: &mut Program, specs: &mut Specializations) -> bool {
    let ro = readonly::ReadOnly::of(program);
    let mut changed = false;
    // Clones appended during the loop are already rewritten when they are made.
    let count = program.funcs.len();
    for fi in 0..count {
        let facts = fields::analyze(&program.aggs, &program.funcs[fi], &ro);
        changed |= fields::rewrite(&mut program.funcs[fi], &facts);
        changed |= specs.redirect_calls(program, &ro, FuncId(fi as u32), &facts);
    }
    changed
}

#[cfg(test)]
mod tests;
