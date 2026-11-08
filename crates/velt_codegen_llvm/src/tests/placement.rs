//! Codegen units: placement keeps call-graph components and small callees with their callers.

use super::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

use crate::units::{plan, plan_apart, Plan};

/// One function of a [`program`]: symbol, statements, the functions it calls (by index).
struct Spec {
    name: String,
    stmts: usize,
    calls: Vec<usize>,
}

fn spec(name: &str, stmts: usize, calls: &[usize]) -> Spec {
    Spec {
        name: name.into(),
        stmts,
        calls: calls.to_vec(),
    }
}

/// A program of unit-returning functions; the first is the exported `velt_main`.
fn program(specs: &[Spec]) -> Program {
    let mut pb = ProgramBuilder::new();
    let ids: Vec<FuncId> = specs.iter().map(|_| pb.reserve()).collect();
    for (i, s) in specs.iter().enumerate() {
        let mut fb = if i == 0 {
            FuncBuilder::export("velt_main", &[], Unit)
        } else {
            FuncBuilder::internal(&s.name, &[], Unit)
        };
        let mut b = fb.block();
        let x = fb.local(I64);
        for k in 0..s.stmts {
            fb.assign(b, x, Rvalue::Use(int(k as i128, I64)));
        }
        for &c in &s.calls {
            b = fb.call(b, Callee::Func(ids[c]), vec![], None);
        }
        fb.ret(b, Operand::Const(Const::Unit, Unit));
        pb.set(ids[i], fb.finish());
    }
    pb.finish()
}

/// The unit defining each function.
fn owners(plan: &Plan, n: usize) -> Vec<usize> {
    let mut owner = vec![usize::MAX; n];
    for (u, unit) in plan.units.iter().enumerate() {
        for &f in &unit.defines {
            owner[f] = u;
        }
    }
    owner
}

/// Weight of each unit, as `units` measures it.
fn unit_weights(p: &Program, plan: &Plan) -> Vec<usize> {
    let weight = |f: &Function| -> usize { f.blocks.iter().map(|b| b.stmts.len() + 1).sum() };
    plan.units
        .iter()
        .map(|u| u.defines.iter().map(|&f| weight(&p.funcs[f])).sum())
        .collect()
}

/// `velt_main` calls `work0` … `work9`; `work2` calls `callee` (index 11, after all of them).
fn caller_program(callee: Vec<Spec>) -> Program {
    let mut specs = vec![spec("main", 20, &(1..=10).collect::<Vec<_>>())];
    for i in 0..10 {
        let calls: &[usize] = if i == 2 { &[11] } else { &[] };
        specs.push(spec(&format!("work{i}"), 60, calls));
    }
    specs.extend(callee);
    program(&specs)
}

#[test]
fn small_callee_lands_with_its_first_caller() {
    let p = caller_program(vec![spec("helper", 20, &[])]);
    let plan = plan(&p, 3);
    assert_eq!(plan.units.len(), 3);
    let owner = owners(&plan, p.funcs.len());
    // In program order, `helper` would be in the last unit with `work9`.
    assert_eq!(owner[11], owner[3], "{owner:?}");
    assert_ne!(owner[3], owner[10], "{owner:?}");
}

/// A callee too large to join its caller's group (k-nucleotide's `frequencies`) still starts its
/// group next to its caller's, so the units are cut around them.
#[test]
fn large_callee_lands_next_to_its_caller() {
    let p = caller_program(vec![spec("frequencies", 600, &[])]);
    let plan = plan(&p, 3);
    let owner = owners(&plan, p.funcs.len());
    assert_eq!(owner[11], owner[3], "{owner:?}");
    assert_ne!(owner[11], owner[10], "{owner:?}");
}

/// Mutually recursive functions at the end of the program (drop glue of a recursive type) form
/// one component, placed with its first caller.
#[test]
fn recursive_chain_lands_with_its_first_caller() {
    let p = caller_program(vec![
        spec("drop_node", 10, &[12]),
        spec("objdrop_node", 10, &[13]),
        spec("drop_option_node", 10, &[11]),
    ]);
    let plan = plan(&p, 3);
    let owner = owners(&plan, p.funcs.len());
    for f in 11..=13 {
        assert_eq!(owner[f], owner[3], "{owner:?}");
    }
    // Nothing but the unit of `work2` refers to the chain.
    for (u, unit) in plan.units.iter().enumerate() {
        if u != owner[3] {
            assert!(unit.declares.iter().all(|&f| f < 11), "unit {u}: {unit:?}");
        }
    }
}

/// A `main` calling many small functions takes at most half a unit's share of them: the units
/// stay balanced.
#[test]
fn groups_are_capped() {
    let n = 40;
    let mut specs = vec![spec("main", 100, &(1..=n).collect::<Vec<_>>())];
    specs.extend((0..n).map(|i| spec(&format!("f{i}"), 100, &[])));
    let p = program(&specs);
    let plan = plan(&p, 4);
    let weights = unit_weights(&p, &plan);
    let total: usize = weights.iter().sum();
    assert_eq!(weights.len(), 4);
    let owner = owners(&plan, p.funcs.len());
    let with_main = owner.iter().filter(|&&u| u == owner[0]).count();
    assert!(with_main <= 1 + n / 4, "{owner:?}");
    for w in &weights {
        assert!(*w * 4 <= total * 3 / 2, "unbalanced: {weights:?}");
    }
}

/// A huge `main` is a unit of its own; the functions it calls are spread over the others.
#[test]
fn huge_main_stays_balanced() {
    let n = 30;
    let mut specs = vec![spec("main", 20_000, &(1..=n).collect::<Vec<_>>())];
    specs.extend((0..n).map(|i| spec(&format!("f{i}"), 300, &[])));
    let p = program(&specs);
    let plan = plan(&p, 4);
    let owner = owners(&plan, p.funcs.len());
    assert_eq!(plan.units.len(), 4);
    assert_eq!(plan.units[owner[0]].defines, [0]);
    let weights = unit_weights(&p, &plan);
    let others: Vec<usize> = (0..4)
        .filter(|&u| u != owner[0])
        .map(|u| weights[u])
        .collect();
    let (lo, hi) = (others.iter().min(), others.iter().max());
    assert!(
        hi.zip(lo).is_some_and(|(h, l)| h - l <= 301 * 2),
        "{weights:?}"
    );
}

/// A call chain thousands of functions deep (`chain_1000`) is placed without recursion.
#[test]
fn deep_chains_are_placed() {
    let n = 100_000;
    let mut specs = vec![spec("main", 1, &[1])];
    specs.extend((1..n).map(|i| {
        let next: &[usize] = if i + 1 < n { &[i + 1] } else { &[] };
        spec(&format!("g{i}"), 1, next)
    }));
    let p = program(&specs);
    let plan = plan(&p, 4);
    assert_eq!(plan.units.len(), 4);
}

/// One component holding most of the program (sigx's any glue and vtables tie half of its IR
/// into one) is cut into pieces: the units stay balanced, where it used to be one unit.
#[test]
fn large_components_are_cut() {
    // `velt_main` calls `c1`; `c1` … `c40` call each other in a ring, 1,000 statements each.
    let n = 40;
    let mut specs = vec![spec("main", 10, &[1])];
    specs.extend((1..=n).map(|i| spec(&format!("c{i}"), 1_000, &[i % n + 1])));
    let p = program(&specs);
    let plan = plan(&p, 4);
    let weights = unit_weights(&p, &plan);
    let total: usize = weights.iter().sum();
    assert_eq!(weights.len(), 4);
    for w in &weights {
        assert!(*w * 4 <= total * 3 / 2, "unbalanced: {weights:?}");
    }
    // The pieces are runs of the ring in program order.
    let owner = owners(&plan, p.funcs.len());
    let changes = owner.windows(2).filter(|w| w[0] != w[1]).count();
    assert!(changes <= 4, "{owner:?}");
}

/// The root module's functions (`main`, a top-level `helper`, a method `P.get`) of a split
/// program get a unit of their own, the last, and the library units are placed as if they were
/// not there: an edit of the entry file (here its functions grown, and one more of them) leaves
/// every library unit as it was, so their modules come from the build cache.
#[test]
fn the_root_module_gets_a_unit_of_its_own() {
    // `velt_main` → `main` → `helper` (top level: `_V6helper`), which calls the library:
    // `lib0` … `lib7`, each calling the next and a leaf of its own.
    // With `extra`, one more function of `main` comes before the library's, as lowering adds
    // a new function where its module's are: every library function's index moves up by one.
    let specs = |main_stmts: usize, extra: bool| {
        let lib = if extra { 4 } else { 3 };
        let mut s = vec![
            spec("velt_main", 2, &[1]),
            spec("_V4main", main_stmts, &[2]),
            spec("_V6helper", main_stmts, &[lib, lib + 2, lib + 4, lib + 6]),
        ];
        if extra {
            s.push(spec("_V1PM3get", main_stmts, &[lib]));
        }
        for k in 0..8 {
            let next = match k {
                7 => vec![lib + 1 + 2 * k],
                _ => vec![lib + 2 * (k + 1), lib + 1 + 2 * k],
            };
            s.push(spec(&format!("_V3stdP3libN4lib{k}"), 3_000, &next));
            s.push(spec(&format!("_V3stdP3libN4leaf{k}"), 2_000, &[]));
        }
        s
    };
    let before = program(&specs(10, false));
    let after = program(&specs(4_000, true));
    let (plan_before, plan_after) = (plan_apart(&before, 4), plan_apart(&after, 4));
    let own = |p: &Program| -> Vec<usize> {
        (0..p.funcs.len())
            .filter(|&f| crate::units::is_root_module(&p.funcs[f].symbol))
            .collect()
    };
    for (p, pl) in [(&before, &plan_before), (&after, &plan_after)] {
        let last = pl.units.last().expect("units");
        assert_eq!(
            last.defines,
            own(p),
            "the last unit holds the root module only"
        );
        assert!(
            pl.units.len() >= 3,
            "the library is split too: {:?}",
            pl.units
        );
    }
    // The library units define the same functions (by symbol) and import the same bodies.
    let library = |p: &Program, pl: &Plan| -> Vec<(Vec<String>, Vec<String>)> {
        let names = |fs: &[usize]| fs.iter().map(|&f| p.funcs[f].symbol.clone()).collect();
        let n = pl.units.len() - 1;
        (pl.units[..n].iter())
            .map(|u| (names(&u.defines), names(&u.imports)))
            .collect()
    };
    assert_eq!(library(&before, &plan_before), library(&after, &plan_after));
}

/// Which symbols are the root module's (`units::is_root_module`): items named without a module,
/// their members, instances, suffixes and closures; not other modules', std's, glue or the entry.
#[test]
fn root_module_symbols() {
    use crate::units::is_root_module;
    for own in [
        "_V4main",
        "_V6helper",
        "_V1PM3get",
        "_V3sum_T3i64",
        "_V4main$poll",
        "_V4mainN17_7bclosure_230_7d",
        "_V6helperN17_7bclosure_230_7d",
        "_V7_23main",
        "_V7_23mainN17_7bclosure_230_7d",
    ] {
        assert!(is_root_module(own), "{own} is the root module's");
    }
    for other in [
        "_V4utilN3foo",
        "_V4utilN1fN17_7bclosure_230_7d",
        "_V3stdP2fsN8readFile",
        "_V13http_2dserverP2h2P8outboundN10pumpStream",
        "_V3libP1mN7_23main",
        "_Gdrop_5Point",
        "velt_main",
        "velt_rt_panic",
        "_V9broken",
    ] {
        assert!(!is_root_module(other), "{other} is not the root module's");
    }
}
