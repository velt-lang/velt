//! The inliner's growth budget: how it is shared out among callers (module docs, #2753).

use super::*;

/// A hot loop's small callee stays inlined however much other code the program holds: code that
/// wants more of the budget than is left (here an uncalled function with 200 calls of a small
/// callee, glue a library may have) is cut to its share, or as the standard library's gets none,
/// and visited first or last it doesn't take the loop's (#2753).
#[test]
fn other_code_does_not_take_a_hot_loops_share_of_the_budget() {
    for name in ["glue", "_V3stdP4glueN4glue"] {
        for cold_first in [false, true] {
            for with_cold in [false, true] {
                let mut pb = ProgramBuilder::new();
                let small = pb.add(adder("small", 35, Linkage::Internal));
                let hot = pb.add(adder("hot", 100, Linkage::Internal));
                let cold = |pb: &mut ProgramBuilder| {
                    pb.add(caller_named(name, &[small], 200, Linkage::Export))
                };
                if with_cold && cold_first {
                    cold(&mut pb);
                }
                let main = pb.add(looping_caller(&[hot]));
                if with_cold && !cold_first {
                    cold(&mut pb);
                }
                let mut p = pb.finish();
                inline(&mut p);
                assert_eq!(
                    calls_to(&p.funcs[main.0 as usize], hot),
                    0,
                    "{name}, with_cold {with_cold}, cold_first {cold_first}"
                );
            }
        }
    }
}

/// The standard library's functions get nothing set aside: among themselves they spend first
/// come, first served, so the first visited keeps what it takes. Shared out, two instances that
/// each want four calls would get two each, which moved the budget from instances whose inlined
/// code folds away to those whose code stays (module docs).
#[test]
fn the_standard_librarys_functions_spend_first_come_first_served() {
    // Its Velt functions, and its C exports (a scheduler thread's entry).
    for (a, b) in [
        ("_V3stdP3libN1a", "_V3stdP3libN1b"),
        ("std_sched_a", "std_sched_b"),
    ] {
        let mut pb = ProgramBuilder::new();
        let small = pb.add(adder("small", 35, Linkage::Internal));
        let a = pb.add(caller_named(a, &[small], 4, Linkage::Export));
        let b = pb.add(caller_named(b, &[small], 4, Linkage::Export));
        let mut p = pb.finish();
        let mut budget = Budget::for_program(&p);
        budget.remaining = 4 * cost(&p.funcs[small.0 as usize]);
        run(&mut p, &mut budget, crate::Served::default());
        assert_valid(&p);
        let left = |f: FuncId| calls_to(&p.funcs[f.0 as usize], small);
        assert_eq!(
            (left(a).min(left(b)), left(a).max(left(b))),
            (0, 4),
            "one keeps all four calls, the other inlines all four"
        );
    }
}

/// What a hot loop wants counts its callee at the size the callee will have when the loop's
/// function is visited: `wrapper`'s own visit inlines `inner` first, and a share set aside for
/// `wrapper` as it was would not pay for it, the rest going to a library function visited before
/// `main`.
#[test]
fn a_callee_counts_at_the_size_it_grows_to_before_its_caller_is_visited() {
    for hog_first in [false, true] {
        let mut pb = ProgramBuilder::new();
        let inner = pb.add(adder("inner", 30, Linkage::Export));
        let wrapper = pb.add(caller_named("wrapper", &[inner], 1, Linkage::Internal));
        let small = pb.add(adder("small", 30, Linkage::Internal));
        let hog = |pb: &mut ProgramBuilder| {
            pb.add(caller_named(
                "_V3stdP3libN3hog",
                &[small],
                200,
                Linkage::Export,
            ))
        };
        if hog_first {
            hog(&mut pb);
        }
        let main = pb.add(looping_caller(&[wrapper]));
        if !hog_first {
            hog(&mut pb);
        }
        let mut p = pb.finish();
        let inner_cost = cost(&p.funcs[inner.0 as usize]);
        let grown = cost(&p.funcs[wrapper.0 as usize]) + inner_cost;
        let mut budget = Budget::for_program(&p);
        // `wrapper`'s call of `inner`, `main`'s two calls of the grown `wrapper`, and a little
        // over (splicing adds a few statements), less than one more call of `small`.
        budget.remaining = inner_cost + 2 * grown + 10;
        run(&mut p, &mut budget, crate::Served::default());
        assert_valid(&p);
        assert_eq!(calls_to(&p.funcs[wrapper.0 as usize], inner), 0);
        assert_eq!(
            calls_to(&p.funcs[main.0 as usize], wrapper),
            0,
            "hog_first {hog_first}"
        );
    }
}

/// Each call is inlined whole or not at all: two callers each wanting one call of 100 from a budget
/// of 150 get a level of 75, which neither call fits, yet one of them is inlined (the second takes
/// what the first left) rather than the budget going unspent.
#[test]
fn a_share_too_small_for_a_call_goes_to_the_next_caller() {
    let mut pb = ProgramBuilder::new();
    let hot = pb.add(adder("hot", 99, Linkage::Export));
    let mut first = looping_caller(&[hot]);
    first.symbol = "first".into();
    let first = pb.add(first);
    let mut second = looping_caller(&[hot]);
    second.symbol = "second".into();
    let second = pb.add(second);
    let mut p = pb.finish();
    // One site each: `looping_caller` calls each callee twice, so keep the first call only.
    for f in [first, second] {
        let body = &mut p.funcs[f.0 as usize];
        let call = (body.blocks.iter()).position(|b| matches!(b.term, Terminator::Call { .. }));
        let next = match &body.blocks[call.unwrap()].term {
            Terminator::Call { next, .. } => *next,
            _ => unreachable!(),
        };
        let second_call = next.0 as usize;
        if let Terminator::Call { next, .. } = body.blocks[second_call].term.clone() {
            body.blocks[second_call].term = Terminator::Goto(next);
        }
    }
    assert_valid(&p);
    let size = cost(&p.funcs[hot.0 as usize]);
    let mut budget = Budget::for_program(&p);
    budget.remaining = size * 3 / 2;
    run(&mut p, &mut budget, crate::Served::default());
    assert_valid(&p);
    let left =
        calls_to(&p.funcs[first.0 as usize], hot) + calls_to(&p.funcs[second.0 as usize], hot);
    assert_eq!(left, 1, "one of the two calls inlined");
}
