//! Semantic preservation on the hand-written corpus: every program is interpreted before and
//! after optimization (at both levels) and must return the same result and make the same
//! extern calls; the optimized VIR must pass the strict validator.

mod common;
mod corpus;

use common::builder::count_calls;
use common::validate::{assert_valid, validate};
use velt_opt::interp::{ExternCall, Interp, RecordingHost, Trap};
use velt_opt::{optimize, OptLevel};
use velt_vir::vir::{Program, Rvalue, Stmt, Terminator};

type Outcome = (Result<u64, Trap>, Vec<ExternCall>);

fn execute(p: &Program, entry: &str, args: &[u64]) -> Outcome {
    let mut interp = Interp::new(p, RecordingHost::default());
    let result = interp.call_symbol(entry, args);
    (result, interp.host.calls)
}

fn optimized(p: &Program, level: OptLevel) -> Program {
    let mut p = p.clone();
    optimize(&mut p, level);
    p
}

fn size(p: &Program) -> usize {
    p.funcs
        .iter()
        .map(|f| f.blocks.iter().map(|b| b.stmts.len() + 1).sum::<usize>())
        .sum()
}

#[test]
fn corpus_is_valid_and_runs_cleanly() {
    for case in corpus::all() {
        if let Err(errs) = validate(&case.program) {
            panic!(
                "{}: invalid corpus program:\n  {}",
                case.name,
                errs.join("\n  ")
            );
        }
        for (entry, args) in &case.runs {
            let (result, _) = execute(&case.program, entry, args);
            assert!(
                matches!(result, Ok(_) | Err(Trap::NoReturn(_))),
                "{} {entry}{args:?}: {result:?}",
                case.name
            );
        }
    }
}

#[test]
fn optimization_preserves_semantics() {
    for case in corpus::all() {
        for level in [OptLevel::None, OptLevel::Speed] {
            let opt = optimized(&case.program, level);
            if let Err(errs) = validate(&opt) {
                panic!(
                    "{} {level:?}: invalid output:\n  {}\n{opt}",
                    case.name,
                    errs.join("\n  ")
                );
            }
            for (entry, args) in &case.runs {
                let before = execute(&case.program, entry, args);
                let after = execute(&opt, entry, args);
                assert_eq!(
                    before, after,
                    "{} {level:?} {entry}{args:?}\n{opt}",
                    case.name
                );
            }
            eprintln!(
                "{:<22} {level:?}: size {} -> {}",
                case.name,
                size(&case.program),
                size(&opt)
            );
        }
    }
}

#[test]
fn optimizing_twice_is_stable_and_valid() {
    for case in corpus::all() {
        let once = optimized(&case.program, OptLevel::Speed);
        let twice = optimized(&once, OptLevel::Speed);
        assert_valid(&twice);
        for (entry, args) in &case.runs {
            assert_eq!(
                execute(&once, entry, args),
                execute(&twice, entry, args),
                "{}",
                case.name
            );
        }
    }
}

fn case(name: &str) -> corpus::Case {
    corpus::all()
        .into_iter()
        .find(|c| c.name == name)
        .expect("corpus case")
}

fn main_of(p: &Program) -> &velt_vir::vir::Function {
    p.funcs
        .iter()
        .find(|f| f.symbol == "velt_main")
        .expect("velt_main")
}

#[test]
fn constant_programs_fold_completely() {
    let opt = optimized(&case("ops_on_constants").program, OptLevel::Speed);
    let main = main_of(&opt);
    let computing = main
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|s| !matches!(s, Stmt::Assign(_, Rvalue::Use(_))))
        .count();
    assert_eq!(computing, 0, "{opt}");
    // Only the prints remain: one block per extern call plus the return.
    assert!(main
        .blocks
        .iter()
        .all(|b| !matches!(b.term, Terminator::Branch { .. })));

    let opt = optimized(&case("switches").program, OptLevel::Speed);
    let main = main_of(&opt);
    assert!(
        main.blocks
            .iter()
            .all(|b| matches!(b.term, Terminator::Call { .. } | Terminator::Return(_))),
        "{opt}"
    );
    assert_eq!(opt.funcs.len(), 1, "classify fully inlined and removed");
}

#[test]
fn small_functions_disappear() {
    for name in ["call_chain", "getters", "called_once_big", "indirect_calls"] {
        let c = case(name);
        let opt = optimized(&c.program, OptLevel::Speed);
        let direct_calls: usize = opt.funcs.iter().map(count_calls).sum();
        let externs = opt
            .funcs
            .iter()
            .flat_map(|f| &f.blocks)
            .filter(|b| {
                matches!(
                    &b.term,
                    Terminator::Call {
                        callee: velt_vir::vir::Callee::Extern(_),
                        ..
                    } | Terminator::Call {
                        callee: velt_vir::vir::Callee::Ptr { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            direct_calls, externs,
            "{name}: internal calls remain\n{opt}"
        );
    }
}

#[test]
fn vtable_targets_survive_and_are_renumbered() {
    let opt = optimized(&case("vtable_dispatch").program, OptLevel::Speed);
    assert!(opt.funcs.iter().all(|f| f.symbol != "unused"), "{opt}");
    let targets: Vec<&str> = opt.statics[0]
        .relocs
        .iter()
        .map(|(_, t)| match t {
            velt_vir::vir::Const::Func(id) => opt.funcs[id.0 as usize].symbol.as_str(),
            other => panic!("unexpected target {other:?}"),
        })
        .collect();
    assert_eq!(targets, ["double", "triple"]);
}
