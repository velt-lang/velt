//! Loops whose tests the check in front would not prove: the pass leaves them, or the tests
//! concerned, alone, and what it does copy agrees with the original at every edge.

use super::*;

/// The calls left in `f`.
fn calls(p: &Program) -> usize {
    count_calls(&p.funcs[0])
}

fn twisted(twist: Twist) -> Shape {
    Shape {
        twist,
        ..Shape::new(Lanes::F32)
    }
}

#[test]
fn a_test_after_the_increment_is_kept_in_the_copy() {
    let shape = twisted(Twist::IncrementBeforeB);
    let p = shape.program();
    let (q, changed) = versioned(&p);
    // `a`'s four tests are decided; `b`'s two stay, in both loops.
    assert!(changed);
    assert_eq!(calls(&p), 6);
    assert_eq!(calls(&q), 6 + 2);
    agree(&p, &q, &edges(4, 16));
}

#[test]
fn an_array_the_loop_assigns_keeps_its_tests() {
    let p = twisted(Twist::AssignA).program();
    let (q, changed) = versioned(&p);
    assert!(changed);
    assert_eq!(calls(&q), 6 + 4);
    agree(&p, &q, &edges(4, 16));
}

#[test]
fn an_offset_the_loop_assigns_keeps_its_tests() {
    let p = twisted(Twist::AssignOffset).program();
    let (q, changed) = versioned(&p);
    assert!(changed);
    assert_eq!(calls(&q), 6 + 4);
    agree(&p, &q, &edges(4, 16));
}

#[test]
fn an_index_with_the_counter_twice_keeps_its_tests() {
    let p = twisted(Twist::DoubleCounter).program();
    let (q, changed) = versioned(&p);
    assert!(changed);
    assert_eq!(calls(&q), 6 + 4);
    agree(&p, &q, &edges(4, 16));
}

#[test]
fn a_loop_that_stores_through_a_pointer_is_not_copied() {
    let p = twisted(Twist::StoreLength).program();
    assert!(!versioned(&p).1);
}

#[test]
fn a_loop_with_another_call_is_not_copied() {
    let mut p = Shape::new(Lanes::F32).program();
    let other = p.externs.len();
    p.externs.push(ExternFn {
        symbol: "velt_rt_other".into(),
        ..p.externs[0].clone()
    });
    for b in &mut p.funcs[0].blocks {
        if let Terminator::Call { callee, args, .. } = &mut b.term {
            if args.len() == 2 {
                *callee = Callee::Extern(velt_vir::vir::ExternId(other as u32));
                break;
            }
        }
    }
    assert!(!versioned(&p).1);
}

/// The comparisons left in `f`'s blocks that test a local against a param.
fn compares(p: &Program) -> usize {
    let f = &p.funcs[0];
    let params = f.params.len() as u32;
    (f.blocks.iter().flat_map(|b| &b.stmts))
        .filter(|s| match s {
            Stmt::Assign(_, Rvalue::Binary(BinOp::Lt | BinOp::Le, _, Operand::Copy(n))) => {
                n.local.0 < params
            }
            _ => false,
        })
        .count()
}

#[test]
fn element_checks_against_a_bound_run_without_them_and_agree_at_every_edge() {
    for lanes in [Lanes::F32, Lanes::I8, Lanes::F64] {
        let shape = Shape {
            twist: Twist::Compares,
            ..Shape::new(lanes)
        };
        copies_and_agrees(&shape);
        // The original's six comparisons and the head's; the copy keeps only the head's, and
        // the check in front compares once per bound.
        let p = shape.program();
        assert_eq!(compares(&p), 7);
        assert_eq!(compares(&versioned(&p).0), 7 + 1 + 2);
    }
}

#[test]
fn a_comparison_the_head_implies_is_left_to_llvm() {
    let shape = Shape {
        twist: Twist::HeadImplied,
        head: Head::Lt,
        step: 1,
        b_offsets: vec![],
        ..Shape::new(Lanes::F32)
    };
    assert!(!versioned(&shape.program()).1);
}

#[test]
fn a_bound_the_loop_changes_keeps_its_comparisons() {
    let shape = twisted(Twist::ShrinkBound);
    let p = shape.program();
    let (q, changed) = versioned(&p);
    // `b`'s two comparisons are decided; `a`'s four, against `la`, stay in the copy.
    assert!(changed);
    assert_eq!(compares(&q), 7 + 1 + 4 + 1);
    agree(&p, &q, &edges(4, 16));
}
