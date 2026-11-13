//! Facts about parameters from the calls of a function (design #525, step 4's parameter rule,
//! for constant arguments only): an internal function whose address is never taken is called
//! only by the direct calls in the program, so its parameter holds one of the values they pass.
//! When every call passes a constant there (`run(50000000, false)`), the parameter's fact is
//! the join of those constants, and a loop bounded by it (`i < iterations`) narrows.

use rustc_hash::FxHashMap;

use velt_vir::vir::{Callee, Const, Function, Linkage, Operand, StaticData, Terminator};

use super::fact::Fact;

/// Per function symbol: the facts of its parameters (`None`: any value).
pub(crate) type ParamFacts = FxHashMap<String, Vec<Option<Fact>>>;

/// The parameter facts of `funcs`' internal functions whose every call passes constants.
pub(crate) fn of(funcs: &[Function], statics: &[StaticData]) -> ParamFacts {
    let parts: Vec<Calls> = funcs.iter().map(calls_of).collect();
    merge(funcs, statics, &parts)
}

/// One function's part of [`of`]: the functions whose address it takes, and per function it
/// calls the join of its calls' argument facts (`None`: a non-constant), in the order met.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Calls {
    taken: Vec<u32>,
    calls: Vec<(u32, Vec<Option<Fact>>)>,
}

/// The join of two calls' argument facts.
fn join(old: Vec<Option<Fact>>, new: impl Iterator<Item = Option<Fact>>) -> Vec<Option<Fact>> {
    (old.into_iter().zip(new))
        .map(|(a, b)| a.zip(b).map(|(a, b)| a.join(b)))
        .collect()
}

/// What `f` contributes to [`of`].
pub(crate) fn calls_of(f: &Function) -> Calls {
    let mut out = Calls::default();
    let mut visit = |op: &Operand| {
        if let Operand::Const(Const::Func(id), _) = op {
            out.taken.push(id.0);
        }
    };
    for b in &f.blocks {
        b.stmts
            .iter()
            .for_each(|s| crate::visit::stmt_operands(s, &mut visit));
        crate::visit::term_operands(&b.term, &mut visit);
    }
    for b in &f.blocks {
        if let Terminator::Call {
            callee: Callee::Func(id),
            args,
            ..
        } = &b.term
        {
            let facts = args.iter().map(|a| match a {
                Operand::Const(c, ty) => Fact::of_const(c, *ty),
                Operand::Copy(_) => None,
            });
            match out.calls.iter_mut().find(|(c, _)| *c == id.0) {
                Some((_, old)) => *old = join(std::mem::take(old), facts),
                None => out.calls.push((id.0, facts.collect())),
            }
        }
    }
    out
}

/// [`of`] from each function's [`calls_of`].
pub(crate) fn merge(funcs: &[Function], statics: &[StaticData], parts: &[Calls]) -> ParamFacts {
    let n = funcs.len();
    let mut taken = vec![false; n];
    let mut take = |id: u32| {
        if let Some(t) = taken.get_mut(id as usize) {
            *t = true;
        }
    };
    for s in statics {
        for (_, c) in &s.relocs {
            if let Const::Func(id) = c {
                take(id.0);
            }
        }
    }
    // Per callee: the join of the argument facts of every call (`None` for a non-constant).
    let mut calls: Vec<Option<Vec<Option<Fact>>>> = vec![None; n];
    for part in parts {
        part.taken.iter().for_each(|&id| take(id));
        for (id, facts) in &part.calls {
            let Some(slot) = calls.get_mut(*id as usize) else {
                continue;
            };
            *slot = Some(match slot.take() {
                None => facts.clone(),
                Some(old) => join(old, facts.iter().copied()),
            });
        }
    }
    let mut out = ParamFacts::default();
    for (i, f) in funcs.iter().enumerate() {
        let internal = matches!(f.linkage, Linkage::Internal);
        if let (true, false, Some(facts)) = (internal, taken[i], calls[i].take()) {
            if facts.iter().any(Option::is_some) && facts.len() == f.params.len() {
                out.insert(f.symbol.clone(), facts);
            }
        }
    }
    out
}
