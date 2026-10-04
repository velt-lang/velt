//! Function specialization on constant closure arguments.
//!
//! When a call passes a pointer to an aggregate whose code field is a known function (a
//! closure `{ code, env }`) to a read-only param, the callee is cloned with that param's
//! known fields substituted, and the call is redirected to the clone. Inside the clone the
//! indirect calls through the param become direct (constfold) and then inlinable — what
//! Rust gets by monomorphizing over closure types. Calls in the clone that pass the param on
//! to other read-only params are specialized the same way, so recursive helpers
//! (`introsort` → `introsort`, `partition`, …) form a specialized family.
//!
//! Clones are memoized by (function, param, known fields) for the whole optimization run,
//! so later rounds reuse them instead of cloning clones.

use velt_vir::vir::{Callee, FuncId, Linkage, Local, Operand, Program, Terminator};

use super::fields::{rewrite, Facts, Known};
use super::readonly::{is_bare, ReadOnly};
use crate::inline::cost;

/// Callees bigger than this are not cloned (keeps code growth bounded).
const MAX_CLONE_COST: u64 = 6_000;
/// Clones made per optimization run at most.
const MAX_CLONES: usize = 64;

/// Clones made so far in this optimization run.
#[derive(Default)]
pub(crate) struct Specializations {
    /// (original, param, fields) → clone.
    made: Vec<(FuncId, usize, Known, FuncId)>,
    /// Clone → the (param, fields) substitutions it already has.
    applied: Vec<(FuncId, usize, Known)>,
}

/// A clone waiting for its calls to be redirected.
struct Pending {
    func: FuncId,
    param: usize,
    known: Known,
}

impl Specializations {
    /// Redirect calls in `caller` that pass a pointer with known code to read-only params;
    /// returns whether any call changed.
    pub fn redirect_calls(
        &mut self,
        program: &mut Program,
        ro: &ReadOnly,
        caller: FuncId,
        facts: &Facts,
    ) -> bool {
        let mut pending = Vec::new();
        let changed = self.redirect_in(program, ro, caller, facts, &mut pending);
        while let Some(p) = pending.pop() {
            let facts = Facts {
                pointers: [(Local(p.param as u32), p.known)].into_iter().collect(),
                ..Facts::default()
            };
            self.redirect_in(program, ro, p.func, &facts, &mut pending);
        }
        changed
    }

    fn redirect_in(
        &mut self,
        program: &mut Program,
        ro: &ReadOnly,
        caller: FuncId,
        facts: &Facts,
        pending: &mut Vec<Pending>,
    ) -> bool {
        let mut changed = false;
        for bi in 0..program.funcs[caller.0 as usize].blocks.len() {
            let term = &program.funcs[caller.0 as usize].blocks[bi].term;
            let (mut callee, candidates) = specializable(term, ro, facts);
            // Every known closure argument in turn (`sort(cmp, neg)` passes two): each clone
            // specializes one more param of the previous one.
            for (param, known) in candidates {
                let Some(clone) = self.clone_for(program, callee, param, known, pending) else {
                    continue;
                };
                if clone != callee {
                    callee = clone;
                    let term = &mut program.funcs[caller.0 as usize].blocks[bi].term;
                    if let Terminator::Call { callee, .. } = term {
                        *callee = Callee::Func(clone);
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// The function to call instead of `func` when param `param` points to `known`.
    fn clone_for(
        &mut self,
        program: &mut Program,
        func: FuncId,
        param: usize,
        known: Known,
        pending: &mut Vec<Pending>,
    ) -> Option<FuncId> {
        if self
            .applied
            .iter()
            .any(|(f, p, k)| *f == func && *p == param && *k == known)
        {
            return Some(func);
        }
        if let Some(&(.., clone)) = self
            .made
            .iter()
            .find(|(f, p, k, _)| *f == func && *p == param && *k == known)
        {
            return Some(clone);
        }
        let original = program.funcs.get(func.0 as usize)?;
        if self.made.len() >= MAX_CLONES || cost(original) > MAX_CLONE_COST {
            return None;
        }
        let mut body = original.clone();
        body.symbol = format!("{}$spec{}", original.symbol, self.made.len());
        body.linkage = Linkage::Internal;
        let facts = Facts {
            pointers: [(Local(param as u32), known.clone())].into_iter().collect(),
            ..Facts::default()
        };
        rewrite(&mut body, &facts);
        let clone = FuncId(program.funcs.len() as u32);
        program.funcs.push(body);
        let inherited: Vec<(usize, Known)> = self
            .applied
            .iter()
            .filter(|(f, ..)| *f == func)
            .map(|(_, p, k)| (*p, k.clone()))
            .collect();
        for (p, k) in inherited {
            self.applied.push((clone, p, k));
        }
        self.applied.push((clone, param, known.clone()));
        self.made.push((func, param, known.clone(), clone));
        pending.push(Pending {
            func: clone,
            param,
            known,
        });
        Some(clone)
    }
}

/// A direct call's callee and its arguments that pass a known-code pointer to a read-only
/// param: (param, fields), in argument order. No candidates for anything else.
fn specializable(term: &Terminator, ro: &ReadOnly, facts: &Facts) -> (FuncId, Vec<(usize, Known)>) {
    let Terminator::Call {
        callee: Callee::Func(id),
        args,
        ..
    } = term
    else {
        return (FuncId(0), Vec::new());
    };
    let candidates = args
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let Operand::Copy(place) = a else {
                return None;
            };
            let known = facts
                .pointers
                .get(&place.local)
                .filter(|_| is_bare(a, place.local))?;
            (known.has_code() && ro.get(id.0 as usize, i)).then(|| (i, known.clone()))
        })
        .collect();
    (*id, candidates)
}
