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
fn comparisons_with_the_bound_first_run_without_them_and_agree_at_every_edge() {
    // `la > at + i + k` is strict and `lb >= i + k` is not: reading either the other way is an
    // element past the end.
    for lanes in [Lanes::F32, Lanes::I8] {
        copies_and_agrees(&Shape {
            twist: Twist::ComparesSwapped,
            ..Shape::new(lanes)
        });
    }
}

#[test]
fn differences_against_the_lane_count_run_without_them_and_agree_at_every_edge() {
    // `la - x >= n` and `n - 1 < lb - x`: wrong where `x` passes the length (the subtraction
    // wraps) or where either is read one element too wide.
    for lanes in [Lanes::F32, Lanes::I8, Lanes::F64] {
        copies_and_agrees(&Shape {
            twist: Twist::Differences,
            ..Shape::new(lanes)
        });
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

#[test]
fn the_check_in_front_is_exact_for_every_form_of_the_counter_test() {
    // `f32x4`, a step of 16, `a` read to `at + i + 16` and `b` to `i + 16`. The last `i` a head
    // admits is `dim - gap`; the copy runs where both arrays reach exactly that far, and the
    // original one element short of it.
    let heads = [
        (Head::Le(16), 16),
        (Head::Lt, 1),
        (Head::Ge(16), 16),
        (Head::Gt(15), 16),
        (Head::Gt(0), 1),
    ];
    for (head, gap) in heads {
        let shape = Shape {
            head,
            ..Shape::new(Lanes::F32)
        };
        let p = shape.program();
        let (q, changed) = versioned(&p);
        assert!(changed, "{head:?}");
        let (at, dim) = (5, 64);
        let last = dim - gap;
        let (la, lb) = (at + last + 16, last + 16);
        let (_, tight) = agree(&p, &q, &[[la, lb, at, dim, 0, 0]]);
        assert_eq!(tight, 0, "{head:?}: the copy runs where both fit exactly");
        let (_, short_a) = agree(&p, &q, &[[la - 1, lb, at, dim, 0, 0]]);
        let (_, short_b) = agree(&p, &q, &[[la, lb - 1, at, dim, 0, 0]]);
        assert!(
            short_a > 0 && short_b > 0,
            "{head:?}: one element short runs the original"
        );
    }
}

/// Whether `f` holds the check in front of a copied loop: a comparison with `2^62`.
fn has_check(p: &Program) -> bool {
    let limit = Operand::Const(Const::Int(BOUND_LIMIT.into()), Ty::U64);
    (p.funcs[0].blocks.iter().flat_map(|b| &b.stmts))
        .any(|s| matches!(s, Stmt::Assign(_, Rvalue::Binary(BinOp::Le, _, n)) if *n == limit))
}

#[test]
fn the_pipeline_copies_a_loop_of_comparisons_in_a_program_without_fits() {
    // Element checks only, and no `fits` extern declared at all: a scalar loop, or `f32x8`
    // (whose `fits` is comparisons), in a program with no 128-bit vector load.
    let mut p = Shape {
        twist: Twist::Compares,
        ..Shape::new(Lanes::F32)
    }
    .program();
    p.externs.clear();
    assert_valid(&p);
    let mut q = p.clone();
    crate::optimize(&mut q, crate::OptLevel::Speed);
    assert_valid(&q);
    assert!(has_check(&q), "{:?}", q.funcs[0]);
    agree(&p, &q, &edges(4, 16));
}

#[test]
fn differences_are_decided_exactly_at_the_end_of_each_array() {
    // `f32`, a step of 16, `a` read to `at + i + 16` (`la - (at + i + 12) >= 4`) and `b` to `i + 16`
    // (`3 < lb - (i + 12)`, strict). One element short of either, the original fails a test the
    // copy must not decide.
    let shape = Shape {
        twist: Twist::Differences,
        ..Shape::new(Lanes::F32)
    };
    let p = shape.program();
    let (q, changed) = versioned(&p);
    assert!(changed);
    let (at, dim) = (5, 64);
    let last = dim - 16;
    let (la, lb) = (at + last + 16, last + 16);
    let points = [
        [la, lb, at, dim, 0, 0],
        [la - 1, lb, at, dim, 0, 0],
        [la, lb - 1, at, dim, 0, 0],
        [la + 1, lb + 1, at, dim, 0, 0],
    ];
    agree(&p, &q, &points);
}
