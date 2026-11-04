//! The per-function checks run on several threads: the errors must be the same messages in the
//! same order whatever the number of threads.

use super::*;

/// A program of `n` functions where every seventh has no blocks (an error) and every eleventh
/// returns an aggregate (another error, reported before it), the rest well-formed; plus
/// `velt_main`.
fn program(n: usize) -> Program {
    let mut p = Program {
        aggs: vec![AggLayout {
            name: "string".into(),
            size: 24,
            align: 8,
            fields: vec![(Ty::U64, 0), (Ty::U64, 8), (Ty::U64, 16)],
        }],
        ..Default::default()
    };
    for i in 0..n {
        let ret = if i % 11 == 0 {
            Ty::Agg(AggId(0))
        } else {
            Ty::Unit
        };
        let mut f = Function::new(format!("f{i}"), vec![], ret, Linkage::Internal);
        if i % 7 != 0 {
            f.blocks.push(BasicBlock {
                stmts: vec![],
                term: Terminator::Return(Operand::Const(Const::Unit, Ty::Unit)),
            });
        }
        p.funcs.push(f);
    }
    let mut main = Function::new("velt_main".into(), vec![], Ty::I32, Linkage::Export);
    main.blocks.push(BasicBlock {
        stmts: vec![],
        term: Terminator::Return(Operand::Const(Const::Int(0), Ty::I32)),
    });
    p.funcs.push(main);
    p
}

#[test]
fn errors_are_the_same_on_any_number_of_threads() {
    let p = program(3000);
    let one = verify_program_on(&p, 1).unwrap_err();
    assert!(one.len() > 400, "{} errors", one.len());
    assert!(one[0].starts_with("fn#0 f0:"), "{}", one[0]);
    for jobs in [2, 3, 8, 64] {
        assert_eq!(
            verify_program_on(&p, jobs).unwrap_err(),
            one,
            "{jobs} threads"
        );
    }
}

#[test]
fn a_well_formed_program_passes_on_any_number_of_threads() {
    let mut p = program(1000);
    p.funcs
        .retain(|f| !f.blocks.is_empty() && !matches!(f.ret, Ty::Agg(_)));
    for jobs in [1, 8] {
        assert_eq!(verify_program_on(&p, jobs), Ok(()), "{jobs} threads");
    }
}
