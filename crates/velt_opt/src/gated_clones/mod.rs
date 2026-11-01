//! Clones of checked callees for gated functions (#1330). A gated function (`Function::target`)
//! runs only where the CPU has its extension, so in an ordinary function it calls, the check of
//! that extension (`velt_simd_has_<ext>`) is true. Such a callee (a `velt/simd` method: the
//! instruction behind the check, the emulation after it) is cloned for the extension with the
//! check replaced by `true` and folded, so the clone is the bare instruction and inlines as a
//! trivial callee. Without the clone, the method inlines only while it is small or called once,
//! and otherwise keeps the check and the emulation inside the gated function.
//!
//! Cloned are ordinary callees that check the extension themselves or through ordinary callees
//! of their own (which the clone's own calls then reach as clones too). Each callee is cloned
//! once per extension.

use rustc_hash::FxHashMap;

use velt_vir::simd::{Ext, SimdCall};
use velt_vir::vir::{
    Callee, Const, FuncId, Linkage, Operand, Program, Rvalue, Stmt, Terminator, Ty,
};

use crate::callgraph::{direct_calls, signatures};
use crate::{constfold, copyprop, dce, simplify_cfg, srclocs};

/// Redirect gated functions' calls of checking callees to their clones; returns whether any
/// call was redirected.
pub(crate) fn run(program: &mut Program) -> bool {
    let mut clones: FxHashMap<(FuncId, Ext), FuncId> = FxHashMap::default();
    let mut made = Vec::new();
    for ext in Ext::ALL {
        let checks = checking(program, ext);
        let mut pending: Vec<usize> = (0..program.funcs.len())
            .filter(|&f| program.funcs[f].target == Some(ext))
            .collect();
        while let Some(fi) = pending.pop() {
            for bi in 0..program.funcs[fi].blocks.len() {
                let Terminator::Call {
                    callee: Callee::Func(callee),
                    ..
                } = program.funcs[fi].blocks[bi].term
                else {
                    continue;
                };
                if !checks.get(callee.0 as usize).copied().unwrap_or(false) {
                    continue;
                }
                let clone = *clones.entry((callee, ext)).or_insert_with(|| {
                    let id = clone_for(program, callee, ext);
                    pending.push(id.0 as usize);
                    made.push(id.0 as usize);
                    id
                });
                if let Terminator::Call { callee, .. } = &mut program.funcs[fi].blocks[bi].term {
                    *callee = Callee::Func(clone);
                }
            }
        }
    }
    if made.is_empty() {
        return false;
    }
    let signatures = signatures(program);
    for fi in made {
        let func = &mut program.funcs[fi];
        constfold::run(&signatures, func);
        copyprop::run(func);
        dce::run(&program.aggs, func);
        simplify_cfg::run(func);
    }
    true
}

/// Per function: whether it is ordinary and checks `ext`, itself or through an ordinary callee.
fn checking(program: &Program, ext: Ext) -> Vec<bool> {
    let checks_itself = |f: &velt_vir::vir::Function| {
        f.blocks.iter().any(|b| {
            matches!(&b.term, Terminator::Call { callee: Callee::Extern(id), .. }
                if is_check(program, *id, ext))
        })
    };
    let ordinary = |f: &velt_vir::vir::Function| f.target.is_none() && !f.is_poll;
    let mut checks: Vec<bool> = program
        .funcs
        .iter()
        .map(|f| ordinary(f) && checks_itself(f))
        .collect();
    loop {
        let mut changed = false;
        for (fi, f) in program.funcs.iter().enumerate() {
            if !checks[fi] && ordinary(f) && direct_calls(f).any(|c| checks[c.0 as usize]) {
                checks[fi] = true;
                changed = true;
            }
        }
        if !changed {
            return checks;
        }
    }
}

fn is_check(program: &Program, id: velt_vir::vir::ExternId, ext: Ext) -> bool {
    program
        .externs
        .get(id.0 as usize)
        .and_then(|e| SimdCall::of_symbol(&e.symbol))
        == Some(SimdCall::Has(ext))
}

/// A copy of `func` gated on `ext`, with its checks of `ext` replaced by `true`.
fn clone_for(program: &mut Program, func: FuncId, ext: Ext) -> FuncId {
    let mut body = program.funcs[func.0 as usize].clone();
    body.symbol = format!("{}${}", body.symbol, ext.name().replace('-', "_"));
    body.linkage = Linkage::Internal;
    body.target = Some(ext);
    for bi in 0..body.blocks.len() {
        let Terminator::Call {
            callee: Callee::Extern(id),
            dest,
            next,
            ..
        } = &body.blocks[bi].term
        else {
            continue;
        };
        if !is_check(program, *id, ext) {
            continue;
        }
        let (dest, next) = (dest.clone(), *next);
        if let Some(dest) = dest {
            let at = body.locs.get(bi).and_then(|l| l.last().copied().flatten());
            let yes = Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool));
            srclocs::push_stmt(&mut body, bi, Stmt::Assign(dest, yes), at);
        }
        body.blocks[bi].term = Terminator::Goto(next);
    }
    let id = FuncId(program.funcs.len() as u32);
    program.funcs.push(body);
    id
}

#[cfg(test)]
mod tests;
