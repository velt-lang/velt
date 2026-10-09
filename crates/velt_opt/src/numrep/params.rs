//! Facts about parameters from the calls of a function (design #525, step 4's parameter rule,
//! for constant arguments only): an internal function whose address is never taken is called
//! only by the direct calls in the program, so its parameter holds one of the values they pass.
//! When every call passes a constant there (`run(50000000, false)`), the parameter's fact is
//! the join of those constants, and a loop bounded by it (`i < iterations`) narrows.

use std::collections::HashMap;

use velt_vir::vir::{Callee, Const, Function, Linkage, Operand, StaticData, Terminator};

use super::fact::Fact;

/// Per function symbol: the facts of its parameters (`None`: any value).
pub(super) type ParamFacts = HashMap<String, Vec<Option<Fact>>>;

/// The parameter facts of `funcs`' internal functions whose every call passes constants.
pub(super) fn of(funcs: &[Function], statics: &[StaticData]) -> ParamFacts {
    let n = funcs.len();
    let mut taken = vec![false; n];
    let mut take = |c: &Const| {
        if let Const::Func(id) = c {
            if let Some(t) = taken.get_mut(id.0 as usize) {
                *t = true;
            }
        }
    };
    for s in statics {
        s.relocs.iter().for_each(|(_, c)| take(c));
    }
    // Per callee: the join of the argument facts of every call (`None` for a non-constant).
    let mut calls: Vec<Option<Vec<Option<Fact>>>> = vec![None; n];
    for f in funcs {
        let mut visit = |op: &Operand| {
            if let Operand::Const(c, _) = op {
                take(c);
            }
        };
        for b in &f.blocks {
            b.stmts
                .iter()
                .for_each(|s| crate::visit::stmt_operands(s, &mut visit));
            crate::visit::term_operands(&b.term, &mut visit);
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
                let slot = &mut calls[id.0 as usize];
                *slot = Some(match slot.take() {
                    None => facts.collect(),
                    Some(old) => old
                        .into_iter()
                        .zip(facts)
                        .map(|(a, b)| a.zip(b).map(|(a, b)| a.join(b)))
                        .collect(),
                });
            }
        }
    }
    let mut out = ParamFacts::new();
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
