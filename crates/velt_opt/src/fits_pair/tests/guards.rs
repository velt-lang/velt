//! The guards a wrong merge would get past, one shape each, and the counts the pass keeps.

use super::*;
use velt_vir::vir::{ParamAttrs, StaticData, StaticId};

#[test]
fn an_address_computed_in_the_second_block_stays() {
    let grid = edges(16);
    // `fits(a, j) && fits(this.b, j)`: the header is inline in the object, so the array is an
    // address `r` computes, with no load.
    let p = shaped(|p, b| {
        let array = p.fb.local(Ty::Ptr);
        let place = deref(p.pb, Ty::Agg(p.agg));
        p.fb.assign(b.r, array, Rvalue::AddrOf(place));
        second_test_of(p, b, copy_local(array));
    });
    assert!(!rewrites(&p, &grid));
    // Not arithmetic, even where the tests do not use it.
    let p = shaped(|p, b| {
        let unused = p.fb.local(Ty::Ptr);
        let place = deref(p.pb, Ty::Agg(p.agg));
        p.fb.assign(b.r, unused, Rvalue::AddrOf(place));
    });
    assert!(!rewrites(&p, &grid));
}

#[test]
fn a_constant_null_second_array_stays() {
    // The first test fails on all of these, so the second array is never read. A known null isn't
    // worth the rewrite and its test against it.
    let grid: Vec<[u64; 3]> = edges(16)
        .into_iter()
        .filter(|[la, _, _]| *la < 16)
        .collect();
    let p = shaped(|p, b| second_test_of(p, b, int(0, Ty::Ptr)));
    assert!(!rewrites(&p, &grid));
}

#[test]
fn a_static_second_array_rewrites() {
    let mut p = shaped(|p, b| {
        let array = Operand::Const(Const::Static(StaticId(0)), Ty::Ptr);
        second_test_of(p, b, array);
    });
    let mut bytes = vec![0u8; 24];
    bytes[8] = 64;
    p.statics.push(StaticData {
        bytes,
        align: 8,
        ..Default::default()
    });
    assert!(rewrites(&p, &edges(16)));
}

#[test]
fn an_index_overwritten_after_its_copy_stays() {
    // `a: _k = _j; _j = *p; fits(x, _k)`, then `r: fits(y, _j)`: the same local, another value.
    let p = shaped(|p, b| {
        let (start, j, k) = (p.entry, p.j, p.fb.local(Ty::U64));
        let len = copy_place(length_of(p.pb, p.agg));
        p.fb.assign(start, k, Rvalue::Use(copy_local(j)));
        p.fb.assign(start, j, Rvalue::Use(len));
        first_test_at(p, b, copy_local(k));
    });
    assert!(!rewrites(&p, &edges(16)));
}

#[test]
fn indexes_read_through_one_pointer_stay() {
    // Two fields behind one local: a place with a projection is no value of that local.
    let p = shaped(|p, b| {
        let mut capacity = deref(p.pb, Ty::Agg(p.agg));
        capacity.proj.push(velt_vir::vir::Proj::Field(2));
        let start = p.entry;
        p.fb.push(
            start,
            Stmt::Assign(capacity.clone(), Rvalue::Use(int(5, Ty::U64))),
        );
        first_test_at(p, b, copy_place(length_of(p.pb, p.agg)));
        second_test_at(p, b, copy_place(capacity));
    });
    assert!(!rewrites(&p, &edges(16)));
}

#[test]
fn a_branch_on_something_else_stays() {
    let grid = edges(16);
    let other = |p: &mut Parts| {
        let (z, start) = (p.fb.local(Ty::Bool), p.entry);
        let test = bin(BinOp::Ne, copy_local(p.j), int(7, Ty::U64));
        p.fb.assign(start, z, test);
        z
    };
    // The first test's result is what `no` returns, and something else decides the way.
    let p = shaped(|p, b| {
        let z = other(p);
        p.fb.branch(b.test1, z, b.r, p.no);
        return_from_no(p, b.x);
    });
    assert!(!rewrites(&p, &grid));
    // The same for the second test's.
    let p = shaped(|p, b| {
        let z = other(p);
        p.fb.branch(b.test2, z, p.yes, p.no);
        return_from_yes(p, b.y);
    });
    assert!(!rewrites(&p, &grid));
}

#[test]
fn one_constant_at_two_types_is_two_indexes() {
    // -1 as a `u8` is 255 and as an `i8` is the top of the range, once both are `u64`.
    let typed = |ty: Ty| {
        move |fb: &mut FuncBuilder, b: BlockId, _: Local| {
            let (n, at) = (fb.local(ty), fb.local(Ty::U64));
            fb.assign(b, n, Rvalue::Use(int(-1, ty)));
            fb.assign(b, at, Rvalue::Cast(copy_local(n), Ty::U64));
            copy_local(at)
        }
    };
    let (lanes, grid) = (Lanes::I8, edges(16));
    let p = pair_program(lanes, lanes, typed(Ty::U8), typed(Ty::I8));
    assert!(!rewrites(&p, &grid));
}

#[test]
fn a_result_read_through_a_pointer_stays() {
    let grid = edges(16);
    // Where the first test holds and the second does not, `*q` is still `true`.
    let through = |p: &mut Parts, l: Local, at: BlockId| {
        let (q, v, start) = (p.fb.local(Ty::Ptr), p.fb.local(Ty::I64), p.entry);
        p.fb.assign(start, q, Rvalue::AddrOf(Place::local(l)));
        let read = copy_place(deref(q, Ty::Bool));
        p.fb.assign(at, v, Rvalue::Cast(read, Ty::I64));
        p.fb.ret(at, copy_local(v));
    };
    let p = shaped(|p, b| {
        let no = p.no;
        through(p, b.x, no);
    });
    assert!(!rewrites(&p, &grid));
    let p = shaped(|p, b| {
        let yes = p.yes;
        through(p, b.y, yes);
    });
    assert!(!rewrites(&p, &grid));
}

#[test]
fn one_local_for_both_results_stays() {
    let p = shaped(|p, b| {
        let call = Terminator::Call {
            callee: p.fits.clone(),
            args: vec![copy_local(p.pb), copy_local(p.j)],
            dest: Some(Place::local(b.x)),
            next: b.test2,
            unwind: None,
        };
        p.fb.term(b.r, call);
        p.fb.branch(b.test2, b.x, p.yes, p.no);
    });
    assert!(!rewrites(&p, &edges(16)));
}

#[test]
fn a_local_the_second_block_writes_with_its_address_taken_stays() {
    // `no` sees `r`'s write through `q`.
    let p = shaped(|p, b| {
        let (t, q, v) = (
            p.fb.local(Ty::I64),
            p.fb.local(Ty::Ptr),
            p.fb.local(Ty::I64),
        );
        let (start, no) = (p.entry, p.no);
        p.fb.assign(start, t, Rvalue::Use(int(7, Ty::I64)));
        p.fb.assign(start, q, Rvalue::AddrOf(Place::local(t)));
        p.fb.assign(b.r, t, Rvalue::Use(int(8, Ty::I64)));
        p.fb.assign(no, v, Rvalue::Use(copy_place(deref(q, Ty::I64))));
        p.fb.ret(no, copy_local(v));
    });
    assert!(!rewrites(&p, &edges(16)));
}

#[test]
fn an_aggregate_the_second_block_copies_stays() {
    let p = shaped(|p, b| {
        let ty = Ty::Agg(p.agg);
        let (from, to, start) = (p.fb.local(ty), p.fb.local(ty), p.entry);
        p.fb.push(
            start,
            Stmt::Assign(field(from, 1), Rvalue::Use(int(1, Ty::U64))),
        );
        p.fb.push(
            start,
            Stmt::Assign(field(from, 0), Rvalue::Use(int(0, Ty::Ptr))),
        );
        p.fb.push(
            start,
            Stmt::Assign(field(from, 2), Rvalue::Use(int(0, Ty::U64))),
        );
        p.fb.assign(b.r, to, Rvalue::Use(copy_local(from)));
    });
    assert!(!rewrites(&p, &edges(16)));
}

/// In code nothing reaches, one test is its own second test. That rewrites nothing that runs, so
/// the results cannot differ; the pair is four blocks or none.
#[test]
fn a_test_that_is_its_own_second_test_stays() {
    let mut pb = ProgramBuilder::new();
    let agg = header(&mut pb);
    let e = fits_ext(&mut pb, Lanes::I8);
    let mut p = parts(agg, e.clone());
    p.fb.ret(p.entry, int(2, Ty::I64));
    let x = p.fb.local(Ty::Bool);
    let a = p.fb.block();
    let test =
        p.fb.call(a, e, vec![copy_local(p.pa), copy_local(p.j)], Some(x));
    p.fb.branch(test, x, a, p.no);
    pb.add(p.fb.finish());
    assert!(!rewrites(&pb.finish(), &edges(16)));
}

/// `f` calls `g`, whose entry block is the second test: `r` has one edge in and the way in that
/// every function has. The rewrite would leave it running from the entry, with the same results,
/// so this pins that the entry never counts as entered once.
#[test]
fn a_second_test_in_the_entry_block_stays() {
    let mut pb = ProgramBuilder::new();
    let agg = header(&mut pb);
    let e = fits_ext(&mut pb, Lanes::I8);
    let params = [Ty::Ptr, Ty::Ptr, Ty::U64, Ty::Bool];
    let mut g = FuncBuilder::internal("g", &params, Ty::I64);
    let (a0, b0, j, again) = (g.param(0), g.param(1), g.param(2), g.param(3));
    let (x, y, go) = (g.local(Ty::Bool), g.local(Ty::Bool), g.local(Ty::Bool));
    let r = g.block();
    let r_test = g.call(r, e.clone(), vec![copy_local(b0), copy_local(j)], Some(y));
    let (yes, a, done, no) = (g.block(), g.block(), g.block(), g.block());
    g.branch(r_test, y, yes, no);
    // Around the loop once, so that both tests run from `a` too.
    g.assign(yes, go, Rvalue::Use(copy_local(again)));
    g.assign(yes, again, Rvalue::Use(boolean(false)));
    g.branch(yes, go, a, done);
    let a_test = g.call(a, e.clone(), vec![copy_local(a0), copy_local(j)], Some(x));
    g.branch(a_test, x, r, no);
    g.ret(done, int(1, Ty::I64));
    g.ret(no, int(2, Ty::I64));
    let g = pb.add(g.finish());
    let mut p = parts(agg, e);
    let v = p.fb.local(Ty::I64);
    let args = vec![
        copy_local(p.pa),
        copy_local(p.pb),
        copy_local(p.j),
        boolean(true),
    ];
    let back = p.fb.call(p.entry, Callee::Func(g), args, Some(v));
    p.fb.ret(back, copy_local(v));
    pb.add(p.fb.finish());
    assert!(!rewrites(&pb.finish(), &edges(16)));
}

/// `fits(a, j) && fits(b, j) && fits(a, j) && ...`, `n` tests.
fn chain(n: usize) -> Program {
    let mut pb = ProgramBuilder::new();
    let agg = header(&mut pb);
    let e = fits_ext(&mut pb, Lanes::I8);
    let mut p = parts(agg, e.clone());
    let mut at = p.entry;
    for i in 0..n {
        let x = p.fb.local(Ty::Bool);
        let array = if i % 2 == 0 { p.pa } else { p.pb };
        let args = vec![copy_local(array), copy_local(p.j)];
        let test = p.fb.call(at, e.clone(), args, Some(x));
        at = if i + 1 == n { p.yes } else { p.fb.block() };
        p.fb.branch(test, x, at, p.no);
    }
    pb.add(p.fb.finish());
    pb.finish()
}

/// `c = 0; if (fits(a, j) && fits(b, j)) { c += 1; }`, `n` times, then `return c`.
fn pairs_in_a_row(n: usize) -> Program {
    let mut pb = ProgramBuilder::new();
    let agg = header(&mut pb);
    let e = fits_ext(&mut pb, Lanes::I8);
    let mut p = parts(agg, e.clone());
    let c = p.fb.local(Ty::I64);
    p.fb.assign(p.entry, c, Rvalue::Use(int(0, Ty::I64)));
    let mut at = p.entry;
    for _ in 0..n {
        let (x, y) = (p.fb.local(Ty::Bool), p.fb.local(Ty::Bool));
        let (r, count, next) = (p.fb.block(), p.fb.block(), p.fb.block());
        let args = vec![copy_local(p.pa), copy_local(p.j)];
        let test1 = p.fb.call(at, e.clone(), args, Some(x));
        p.fb.branch(test1, x, r, next);
        let args = vec![copy_local(p.pb), copy_local(p.j)];
        let test2 = p.fb.call(r, e.clone(), args, Some(y));
        p.fb.branch(test2, y, count, next);
        p.fb.assign(count, c, bin(BinOp::Add, copy_local(c), int(1, Ty::I64)));
        p.fb.goto(count, next);
        at = next;
    }
    p.fb.ret(at, copy_local(c));
    pb.add(p.fb.finish());
    pb.finish()
}

#[test]
fn a_chain_of_tests_merges_by_twos() {
    // After the first pair, the third test's block has two edges in until the dead one goes, so
    // the second and third do not pair up. The third and fourth do.
    let p = chain(4);
    let mut q = p.clone();
    assert_eq!(run(&mut q), vec![0]);
    crate::simplify_cfg::run(&mut q.funcs[0]);
    assert_eq!(count_calls(&q.funcs[0]), 2, "{:?}", q.funcs[0]);
    agree(&p, &q, &edges(16));
    // An odd one out keeps its own test.
    let p = chain(3);
    let mut q = p.clone();
    assert_eq!(run(&mut q), vec![0]);
    crate::simplify_cfg::run(&mut q.funcs[0]);
    assert_eq!(count_calls(&q.funcs[0]), 2, "{:?}", q.funcs[0]);
    agree(&p, &q, &edges(16));
}

#[test]
fn many_pairs_in_one_function_all_rewrite() {
    let p = pairs_in_a_row(20);
    let mut q = p.clone();
    assert_eq!(run(&mut q), vec![0]);
    crate::simplify_cfg::run(&mut q.funcs[0]);
    assert_eq!(count_calls(&q.funcs[0]), 20, "{:?}", q.funcs[0]);
    agree(&p, &q, &edges(16));
}

#[test]
fn the_counts_kept_are_what_a_new_scan_gives() {
    for (mut p, rewrites) in [(chain(6), 3), (pairs_in_a_row(5), 5)] {
        let table = fits_table(&p.externs);
        let (func, externs) = (&mut p.funcs[0], &mut p.externs);
        let kept = rewrite_pairs(func, externs, &table).expect("counts");
        assert_eq!(kept.rewrites, rewrites);
        let scan = Counts::of(func);
        assert_eq!(kept.edges, scan.edges);
        let reads = |c: &Counts| c.usage.locals.iter().map(|u| u.reads).collect::<Vec<_>>();
        assert_eq!(reads(&kept), reads(&scan));
    }
}

#[test]
fn a_null_second_array_where_the_first_test_fails_gives_the_same_result() {
    // `lb` = 0 is the second array as a pointer, known to no pass.
    let grid: Vec<[u64; 3]> = edges(16)
        .into_iter()
        .filter(|[la, lb, _]| *la < 16 && *lb == 0)
        .collect();
    assert!(grid.len() > 20);
    let p = shaped(|p, b| {
        let array = p.fb.local(Ty::Ptr);
        let lb = p.fb.param(1);
        p.fb.assign(p.entry, array, Rvalue::Cast(copy_local(lb), Ty::Ptr));
        second_test_of(p, b, copy_local(array));
    });
    assert!(rewrites(&p, &grid));
}

#[test]
fn a_null_second_array_is_false_with_a_long_first_one_too() {
    // The two tests read the null here (#349's area); the one test gives false.
    let p = shaped(|p, b| {
        let array = p.fb.local(Ty::Ptr);
        p.fb.assign(p.entry, array, Rvalue::Cast(int(0, Ty::U64), Ty::Ptr));
        second_test_of(p, b, copy_local(array));
    });
    let mut q = p.clone();
    assert!(!run(&mut q).is_empty());
    let got = Interp::new(&q, Headers::default()).call_symbol("f", &[64, 0, 0]);
    assert_eq!(got, Ok(2));
    let got = Interp::new(&p, Headers::default()).call_symbol("f", &[64, 0, 0]);
    assert!(got.is_err(), "{got:?}");
}

#[test]
fn a_null_first_array_is_read_as_the_first_test_reads_it() {
    let p = shaped(|p, b| {
        let array = p.fb.local(Ty::Ptr);
        let la = p.fb.param(0);
        p.fb.assign(p.entry, array, Rvalue::Cast(copy_local(la), Ty::Ptr));
        let call = Terminator::Call {
            callee: p.fits.clone(),
            args: vec![copy_local(array), copy_local(p.j)],
            dest: Some(Place::local(b.x)),
            next: b.test1,
            unwind: None,
        };
        p.fb.term(p.entry, call);
    });
    let mut q = p.clone();
    assert!(!run(&mut q).is_empty());
    for program in [&p, &q] {
        let got = Interp::new(program, Headers::default()).call_symbol("f", &[0, 64, 0]);
        assert!(got.is_err(), "{got:?}");
    }
}

/// How many null tests `run` wrote in `f`.
fn null_tests(p: &mut Program) -> usize {
    assert!(!run(p).is_empty());
    let stmts = p.funcs[0].blocks.iter().flat_map(|b| &b.stmts);
    let null = int(0, Ty::Ptr);
    stmts
        .filter(|s| matches!(s, Stmt::Assign(_, Rvalue::Binary(BinOp::Ne, _, c)) if *c == null))
        .count()
}

#[test]
fn only_a_second_array_that_may_be_null_is_tested() {
    // A local, whatever it holds.
    let mut p = same_lane_pair(Lanes::I8);
    assert_eq!(null_tests(&mut p), 1);
    // A static's address.
    let mut p = shaped(|p, b| {
        second_test_of(p, b, Operand::Const(Const::Static(StaticId(0)), Ty::Ptr));
    });
    p.statics.push(StaticData {
        bytes: vec![0u8; 24],
        align: 8,
        ..Default::default()
    });
    assert_eq!(null_tests(&mut p), 0);
    // A param: tested unless it promises otherwise, and it can't where it is written again.
    let param = |promise: bool, written: bool| {
        let mut pb = ProgramBuilder::new();
        let fits = fits_ext(&mut pb, Lanes::I8);
        let mut fb = FuncBuilder::new("f", &[Ty::Ptr, Ty::Ptr], Ty::I64, Linkage::Export);
        let (a, b) = (fb.param(0), fb.param(1));
        let (x, y) = (fb.local(Ty::Bool), fb.local(Ty::Bool));
        let first = fb.block();
        let (r, yes, no) = (fb.block(), fb.block(), fb.block());
        if written {
            fb.assign(first, b, Rvalue::Use(copy_local(a)));
        }
        let test = fb.call(
            first,
            fits.clone(),
            vec![copy_local(a), int(0, Ty::U64)],
            Some(x),
        );
        fb.branch(test, x, r, no);
        let test = fb.call(r, fits, vec![copy_local(b), int(0, Ty::U64)], Some(y));
        fb.branch(test, y, yes, no);
        fb.ret(yes, int(1, Ty::I64));
        fb.ret(no, int(2, Ty::I64));
        let mut f = fb.finish();
        if promise {
            f.param_attrs = vec![
                ParamAttrs::default(),
                ParamAttrs {
                    nonnull: true,
                    dereferenceable: 24,
                    ..ParamAttrs::default()
                },
            ];
        }
        pb.add(f);
        pb.finish()
    };
    assert_eq!(null_tests(&mut param(false, false)), 1);
    assert_eq!(null_tests(&mut param(true, false)), 0);
    assert_eq!(null_tests(&mut param(true, true)), 1);
}
