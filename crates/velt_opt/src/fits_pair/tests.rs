//! Each program runs in the reference interpreter before and after the pass, on lengths and
//! indexes up to the edges; the host computes `fits` and `fits_both` from the array headers.

use super::*;
use crate::interp::{Host, Interp, Memory, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{AggId, Linkage};

mod guards;

/// What `fits` and `fits_both` mean, from the headers the arguments point to (the backends'
/// helpers are checked against the same edges by velt's `simd_fits_both` test). Counts header
/// reads through a null pointer as traps, as the machine would.
#[derive(Default)]
struct Headers {
    calls: u32,
}

impl Host for Headers {
    fn call(&mut self, ext: &ExternFn, args: &[u64], mem: &mut Memory) -> Result<u64, Trap> {
        self.calls += 1;
        let Some(SimdCall::Vector(lanes, op)) = SimdCall::of_symbol(&ext.symbol) else {
            return Ok(0);
        };
        let n = u64::from(lanes.count());
        let inside = |len: u64, at: u64| len >= n && at <= len - n;
        Ok(u64::from(match (op, args) {
            (Op::Fits, [a, at]) => inside(mem.read_scalar(a + 8, Ty::U64)?, *at),
            (Op::FitsBoth, [a, b, at]) => {
                let (x, y) = (
                    mem.read_scalar(a + 8, Ty::U64)?,
                    mem.read_scalar(b + 8, Ty::U64)?,
                );
                inside(x.min(y), *at)
            }
            _ => return Err(Trap::Invalid(format!("unexpected {}", ext.symbol))),
        }))
    }
}

fn fits_ext(pb: &mut ProgramBuilder, lanes: Lanes) -> Callee {
    let symbol = format!("velt_simd_{}_fits", lanes.name());
    Callee::Extern(pb.ext(&symbol, &[Ty::Ptr, Ty::U64], Ty::Bool, false))
}

/// The parts of `f(la, lb, j)` the shapes below vary: two array headers with those lengths behind
/// `pa` and `pb`, and the blocks the tests go to (`yes` returns 1, `no` 2).
struct Parts {
    fb: FuncBuilder,
    agg: AggId,
    fits: Callee,
    pa: Local,
    pb: Local,
    j: Local,
    entry: BlockId,
    yes: BlockId,
    no: BlockId,
}

fn header(pb: &mut ProgramBuilder) -> AggId {
    pb.agg(
        "header",
        24,
        8,
        &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)],
    )
}

fn parts(agg: AggId, fits: Callee) -> Parts {
    let mut fb = FuncBuilder::new("f", &[Ty::U64, Ty::U64, Ty::U64], Ty::I64, Linkage::Export);
    let (ha, hb) = (fb.local(Ty::Agg(agg)), fb.local(Ty::Agg(agg)));
    let (pa, pb, j) = (fb.local(Ty::Ptr), fb.local(Ty::Ptr), fb.param(2));
    let entry = fb.block();
    let (la, lb) = (fb.param(0), fb.param(1));
    fb.push(
        entry,
        Stmt::Assign(field(ha, 1), Rvalue::Use(copy_local(la))),
    );
    fb.push(
        entry,
        Stmt::Assign(field(hb, 1), Rvalue::Use(copy_local(lb))),
    );
    fb.assign(entry, pa, Rvalue::AddrOf(Place::local(ha)));
    fb.assign(entry, pb, Rvalue::AddrOf(Place::local(hb)));
    let (yes, no) = (fb.block(), fb.block());
    fb.ret(yes, int(1, Ty::I64));
    fb.ret(no, int(2, Ty::I64));
    Parts {
        fb,
        agg,
        fits,
        pa,
        pb,
        j,
        entry,
        yes,
        no,
    }
}

/// `if (fits(a, j1) && fits(b, j2))`: `index1` / `index2` fill in the two index
/// computations, in the first test's block and the second's; each returns the index operand.
fn pair_program(
    first: Lanes,
    second: Lanes,
    mut index1: impl FnMut(&mut FuncBuilder, BlockId, Local) -> Operand,
    mut index2: impl FnMut(&mut FuncBuilder, BlockId, Local) -> Operand,
) -> Program {
    let mut pb = ProgramBuilder::new();
    let agg = header(&mut pb);
    let (e1, e2) = (fits_ext(&mut pb, first), fits_ext(&mut pb, second));
    let mut p = parts(agg, e1.clone());
    let (x, y) = (p.fb.local(Ty::Bool), p.fb.local(Ty::Bool));
    let j1 = index1(&mut p.fb, p.entry, p.j);
    let test1 = p.fb.call(p.entry, e1, vec![copy_local(p.pa), j1], Some(x));
    let r = p.fb.block();
    p.fb.branch(test1, x, r, p.no);
    let j2 = index2(&mut p.fb, r, p.j);
    let test2 = p.fb.call(r, e2, vec![copy_local(p.pb), j2], Some(y));
    p.fb.branch(test2, y, p.yes, p.no);
    pb.add(p.fb.finish());
    pb.finish()
}

fn same_index(fb: &mut FuncBuilder, b: BlockId, j: Local) -> Operand {
    let at = fb.local(Ty::U64);
    fb.assign(b, at, bin(BinOp::Add, copy_local(j), int(48, Ty::U64)));
    copy_local(at)
}

fn plain(_: &mut FuncBuilder, _: BlockId, j: Local) -> Operand {
    copy_local(j)
}

fn same_lane_pair(lanes: Lanes) -> Program {
    pair_program(lanes, lanes, same_index, same_index)
}

/// Whether the pass rewrites a pair of `p`, whose output must be valid either way, and that `f`
/// agrees before and after on `grid`.
fn rewrites(p: &Program, grid: &[[u64; 3]]) -> bool {
    assert_valid(p);
    let mut q = p.clone();
    let changed = run(&mut q);
    for f in &changed {
        crate::simplify_cfg::run(&mut q.funcs[*f]);
    }
    assert_valid(&q);
    agree(p, &q, grid);
    !changed.is_empty()
}

fn agree(p: &Program, q: &Program, grid: &[[u64; 3]]) {
    for args in grid {
        let before = Interp::new(p, Headers::default()).call_symbol("f", args);
        let after = Interp::new(q, Headers::default()).call_symbol("f", args);
        assert_eq!(after, before, "f{args:?}");
        assert!(before.is_ok(), "f{args:?}: {before:?}");
    }
}

/// Lengths and indexes at the edges for `n` lanes: around 0, the lane count and its double, the
/// other length (`j` near `min`, near `max`), and the top of the range, where `j + 48` and
/// `len - n` would wrap.
fn edges(n: u64) -> Vec<[u64; 3]> {
    let top = u64::MAX;
    let lens = [
        0,
        1,
        n - 1,
        n,
        n + 1,
        2 * n - 1,
        2 * n,
        2 * n + 1,
        47,
        48,
        49,
        64,
        1 << 53,
        (1 << 63) + 48,
        top - n,
        top - n + 1,
        top - 1,
        top,
    ];
    let mut grid = vec![];
    for la in lens {
        for lb in lens {
            let min = la.min(lb);
            let at = [
                0,
                1,
                47,
                48,
                n - 1,
                n,
                n + 1,
                min.wrapping_sub(n),
                min.wrapping_sub(n).wrapping_add(1),
                min.wrapping_sub(n).wrapping_sub(1),
                min.wrapping_sub(n + 48),
                min.wrapping_sub(n + 47),
                min.wrapping_sub(n + 49),
                min,
                la.max(lb).wrapping_sub(n),
                la.max(lb).wrapping_sub(n + 48),
                1 << 63,
                top - 48,
                top - 47,
                top - 47 - n,
                top - n,
                top - n + 1,
                top - 1,
                top,
            ];
            grid.extend(at.map(|j| [la, lb, j]));
        }
    }
    grid
}

#[test]
fn a_pair_of_one_index_is_one_test_with_the_same_result_at_every_edge() {
    for lanes in Lanes::ALL {
        let p = same_lane_pair(lanes);
        let grid = edges(u64::from(lanes.count()));
        assert!(rewrites(&p, &grid), "{lanes:?}");
        // The test does see both outcomes, and both orders of the lengths.
        let (mut yes, mut no) = (0, 0);
        for args in &grid {
            match Interp::new(&p, Headers::default()).call_symbol("f", args) {
                Ok(1) => yes += 1,
                _ => no += 1,
            }
        }
        assert!(yes > 100 && no > 100, "{lanes:?}: {yes} yes, {no} no");
    }
}

#[test]
fn the_rewrite_makes_one_call_and_declares_the_helper_once() {
    let mut p = same_lane_pair(Lanes::I8);
    let tests: Vec<&Vec<Operand>> = (p.funcs[0].blocks.iter())
        .filter_map(|b| match &b.term {
            Terminator::Call { args, .. } => Some(args),
            _ => None,
        })
        .collect();
    let want = vec![
        tests[0][0].clone(),
        tests[1][0].clone(),
        tests[0][1].clone(),
    ];
    let mut second = p.funcs[0].clone();
    second.symbol = "g".to_string();
    p.funcs.push(second);
    assert_eq!(run(&mut p), vec![0, 1]);
    let both: Vec<_> = (p.externs.iter())
        .filter(|e| e.symbol == "velt_simd_i8x16_fits_both")
        .collect();
    assert_eq!(both.len(), 1);
    let args = p.funcs[0].blocks.iter().find_map(|b| match &b.term {
        Terminator::Call { args, .. } if args.len() == 3 => Some(args),
        _ => None,
    });
    let args = args.expect("the call");
    // In the order of the source; `min` does not care, the reader of the VIR does.
    assert_eq!(*args, want);
    // The verifier knows the helper's signature; this program is no whole one (no `velt_main`).
    let errors = velt_vir::verify(&p).err().unwrap_or_default();
    assert!(errors.iter().all(|e| e.contains("velt_main")), "{errors:?}");
    crate::simplify_cfg::run(&mut p.funcs[0]);
    assert_eq!(count_calls(&p.funcs[0]), 1, "{:?}", p.funcs[0]);
}

#[test]
fn the_whole_pipeline_keeps_the_results() {
    let p = same_lane_pair(Lanes::U8);
    let mut q = p.clone();
    crate::optimize(&mut q, crate::OptLevel::Speed);
    assert_valid(&q);
    assert!(q.externs.iter().any(|e| e.symbol.ends_with("_fits_both")));
    agree(&p, &q, &edges(16));
}

#[test]
fn indexes_equal_by_value_are_one_index() {
    let grid = edges(16);
    // The same local, and the same local cast the same way.
    let lanes = Lanes::I8;
    assert!(rewrites(&pair_program(lanes, lanes, plain, plain), &grid));
    let cast = |fb: &mut FuncBuilder, b: BlockId, j: Local| {
        let (narrow, at) = (fb.local(Ty::U32), fb.local(Ty::U64));
        fb.assign(b, narrow, Rvalue::Cast(copy_local(j), Ty::U32));
        fb.assign(b, at, Rvalue::Cast(copy_local(narrow), Ty::U64));
        copy_local(at)
    };
    assert!(rewrites(&pair_program(lanes, lanes, cast, cast), &grid));
    let constant = |_: &mut FuncBuilder, _: BlockId, _: Local| int(16, Ty::U64);
    assert!(rewrites(
        &pair_program(lanes, lanes, constant, constant),
        &grid
    ));
    // `j + 48` in the first block, with one in the second computed from a copy.
    let copied = |fb: &mut FuncBuilder, b: BlockId, j: Local| {
        let (c, at) = (fb.local(Ty::U64), fb.local(Ty::U64));
        fb.assign(b, c, Rvalue::Use(copy_local(j)));
        fb.assign(b, at, bin(BinOp::Add, copy_local(c), int(48, Ty::U64)));
        copy_local(at)
    };
    assert!(rewrites(
        &pair_program(lanes, lanes, same_index, copied),
        &grid
    ));
}

#[test]
fn different_indexes_stay_two_tests() {
    let lanes = Lanes::I8;
    let grid = edges(16);
    let plus = |by: i128| {
        move |fb: &mut FuncBuilder, b: BlockId, j: Local| {
            let at = fb.local(Ty::U64);
            fb.assign(b, at, bin(BinOp::Add, copy_local(j), int(by, Ty::U64)));
            copy_local(at)
        }
    };
    assert!(!rewrites(
        &pair_program(lanes, lanes, plain, plus(1)),
        &grid
    ));
    assert!(!rewrites(
        &pair_program(lanes, lanes, plus(48), plus(49)),
        &grid
    ));
    // One operation apart, operands swapped in a subtraction, one cast to another width.
    let sub = |fb: &mut FuncBuilder, b: BlockId, j: Local| {
        let at = fb.local(Ty::U64);
        fb.assign(b, at, bin(BinOp::Sub, copy_local(j), int(48, Ty::U64)));
        copy_local(at)
    };
    assert!(!rewrites(&pair_program(lanes, lanes, plus(48), sub), &grid));
    let swapped = |fb: &mut FuncBuilder, b: BlockId, j: Local| {
        let at = fb.local(Ty::U64);
        fb.assign(b, at, bin(BinOp::Sub, int(48, Ty::U64), copy_local(j)));
        copy_local(at)
    };
    assert!(!rewrites(&pair_program(lanes, lanes, sub, swapped), &grid));
    let cast = |to: Ty| {
        move |fb: &mut FuncBuilder, b: BlockId, j: Local| {
            let (narrow, at) = (fb.local(to), fb.local(Ty::U64));
            fb.assign(b, narrow, Rvalue::Cast(copy_local(j), to));
            fb.assign(b, at, Rvalue::Cast(copy_local(narrow), Ty::U64));
            copy_local(at)
        }
    };
    assert!(!rewrites(
        &pair_program(lanes, lanes, cast(Ty::U32), cast(Ty::U16)),
        &grid
    ));
}

#[test]
fn different_lane_types_stay_two_tests() {
    // Lengths and indexes where 2, 4, 8 and 16 lanes differ; the pairs are many.
    let mut grid = vec![];
    for la in [0, 2, 4, 8, 16, 32] {
        for lb in [0, 2, 4, 8, 16, 32] {
            grid.extend([0, 2, 4, 14, 16].map(|j| [la, lb, j]));
        }
    }
    for first in Lanes::ALL {
        for second in Lanes::ALL.into_iter().filter(|l| *l != first) {
            let p = pair_program(first, second, same_index, same_index);
            assert!(!rewrites(&p, &grid), "{first:?} {second:?}");
        }
    }
}

/// `if (fits(a, j) && fits(b, j))` with `change` free to edit the function before it is built.
fn shaped(change: impl FnOnce(&mut Parts, Blocks)) -> Program {
    let mut pb = ProgramBuilder::new();
    let agg = header(&mut pb);
    let e = fits_ext(&mut pb, Lanes::I8);
    let mut p = parts(agg, e.clone());
    let (x, y) = (p.fb.local(Ty::Bool), p.fb.local(Ty::Bool));
    let test1 = p.fb.call(
        p.entry,
        e.clone(),
        vec![copy_local(p.pa), copy_local(p.j)],
        Some(x),
    );
    let r = p.fb.block();
    p.fb.branch(test1, x, r, p.no);
    let test2 =
        p.fb.call(r, e, vec![copy_local(p.pb), copy_local(p.j)], Some(y));
    p.fb.branch(test2, y, p.yes, p.no);
    let blocks = Blocks {
        test1,
        r,
        test2,
        x,
        y,
    };
    change(&mut p, blocks);
    pb.add(p.fb.finish());
    pb.finish()
}

#[derive(Clone, Copy)]
struct Blocks {
    test1: BlockId,
    r: BlockId,
    test2: BlockId,
    x: Local,
    y: Local,
}

/// The first test reads `at` instead of `j`.
fn first_test_at(p: &mut Parts, b: Blocks, at: Operand) {
    let call = Terminator::Call {
        callee: p.fits.clone(),
        args: vec![copy_local(p.pa), at],
        dest: Some(Place::local(b.x)),
        next: b.test1,
        unwind: None,
    };
    p.fb.term(p.entry, call);
}

/// The second test reads `array` instead of `pb`.
fn second_test_of(p: &mut Parts, b: Blocks, array: Operand) {
    let call = Terminator::Call {
        callee: p.fits.clone(),
        args: vec![array, copy_local(p.j)],
        dest: Some(Place::local(b.y)),
        next: b.test2,
        unwind: None,
    };
    p.fb.term(b.r, call);
}

/// The second test reads `at` instead of `j`.
fn second_test_at(p: &mut Parts, b: Blocks, at: Operand) {
    let call = Terminator::Call {
        callee: p.fits.clone(),
        args: vec![copy_local(p.pb), at],
        dest: Some(Place::local(b.y)),
        next: b.test2,
        unwind: None,
    };
    p.fb.term(b.r, call);
}

/// `yes` reads local `l`: its value is observable after the tests.
fn return_from_yes(p: &mut Parts, l: Local) {
    let (yes, t, is1) = (p.yes, Ty::I64, p.fb.local(Ty::I64));
    p.fb.assign(yes, is1, Rvalue::Cast(copy_local(l), t));
    p.fb.ret(yes, copy_local(is1));
}

/// `no` reads local `l` the same way.
fn return_from_no(p: &mut Parts, l: Local) {
    let (no, t, is1) = (p.no, Ty::I64, p.fb.local(Ty::I64));
    p.fb.assign(no, is1, Rvalue::Cast(copy_local(l), t));
    p.fb.ret(no, copy_local(is1));
}

#[test]
fn the_plain_shape_rewrites() {
    assert!(rewrites(&shaped(|_, _| {}), &edges(16)));
}

#[test]
fn a_result_read_elsewhere_stays() {
    let grid = edges(16);
    // Where the first test holds and the second does not, the first's result is still `true`.
    let p = shaped(|p, b| return_from_no(p, b.x));
    assert!(!rewrites(&p, &grid));
    let p = shaped(|p, b| return_from_yes(p, b.y));
    assert!(!rewrites(&p, &grid));
}

#[test]
fn what_the_second_block_computes_for_later_stays() {
    let p = shaped(|p, b| {
        let later = p.fb.local(Ty::U64);
        let start = p.entry;
        p.fb.assign(start, later, Rvalue::Use(int(0, Ty::U64)));
        p.fb.assign(
            b.r,
            later,
            bin(BinOp::Add, copy_local(p.j), int(1, Ty::U64)),
        );
        return_from_no(p, later);
    });
    assert!(!rewrites(&p, &edges(16)));
}

#[test]
fn different_false_targets_stay() {
    let p = shaped(|p, b| {
        let other = p.fb.block();
        p.fb.ret(other, int(3, Ty::I64));
        p.fb.branch(b.test2, b.y, p.yes, other);
    });
    assert!(!rewrites(&p, &edges(16)));
}

#[test]
fn a_store_between_the_tests_stays() {
    let grid = edges(16);
    // Through `pb`, into the very length the second test reads.
    let p = shaped(|p, b| {
        let len = length_of(p.pb, p.agg);
        p.fb.push(b.r, Stmt::Assign(len, Rvalue::Use(int(1000, Ty::U64))));
    });
    assert!(!rewrites(&p, &grid));
    // Nothing the tests read, and still not arithmetic.
    let p = shaped(|p, b| {
        let len = length_of(p.pb, p.agg);
        p.fb.push(b.test2, Stmt::Assign(len, Rvalue::Use(int(1000, Ty::U64))));
    });
    assert!(!rewrites(&p, &grid));
}

#[test]
fn a_memory_fill_or_copy_between_the_tests_stays() {
    let grid = edges(16);
    // Zero bytes over the header `pb` points to: the second test then sees length 0.
    let p = shaped(|p, b| {
        let (dst, byte, len) = (copy_local(p.pb), int(0, Ty::U8), int(24, Ty::U64));
        p.fb.push(b.r, Stmt::MemSet { dst, byte, len });
    });
    assert!(!rewrites(&p, &grid));
    // The first array's header over the second's.
    let p = shaped(|p, b| {
        let (dst, src) = (copy_local(p.pb), copy_local(p.pa));
        p.fb.push(b.r, Stmt::MemCopy { dst, src, size: 24 });
    });
    assert!(!rewrites(&p, &grid));
}

#[test]
fn a_trap_or_a_load_between_the_tests_stays() {
    // No length is 0 here, so the division runs: a pass may not drop one that could trap.
    let grid: Vec<[u64; 3]> = edges(16)
        .into_iter()
        .filter(|[la, lb, _]| *la > 0 && *lb > 0)
        .collect();
    let p = shaped(|p, b| {
        let q = p.fb.local(Ty::U64);
        let by = copy_local(p.fb.param(1));
        p.fb.assign(b.r, q, bin(BinOp::Div, copy_local(p.j), by));
    });
    assert!(!rewrites(&p, &grid));
    // A read through a pointer, unused: it could fault where the first test would have failed.
    let p = shaped(|p, b| {
        let v = p.fb.local(Ty::U64);
        let len = copy_place(length_of(p.pb, p.agg));
        p.fb.assign(b.r, v, Rvalue::Use(len));
    });
    assert!(!rewrites(&p, &grid));
}

/// `(*p).1`: the length field of the header `p` points to.
fn length_of(p: Local, agg: AggId) -> Place {
    let mut place = deref(p, Ty::Agg(agg));
    place.proj.push(velt_vir::vir::Proj::Field(1));
    place
}

#[test]
fn a_call_between_the_tests_stays() {
    let mut pb = ProgramBuilder::new();
    let agg = header(&mut pb);
    let e = fits_ext(&mut pb, Lanes::I8);
    let side = Callee::Extern(pb.ext("side", &[], Ty::Unit, false));
    let mut p = parts(agg, e.clone());
    let (x, y) = (p.fb.local(Ty::Bool), p.fb.local(Ty::Bool));
    let test1 = p.fb.call(
        p.entry,
        e.clone(),
        vec![copy_local(p.pa), copy_local(p.j)],
        Some(x),
    );
    let call = p.fb.block();
    p.fb.branch(test1, x, call, p.no);
    let r = p.fb.call(call, side, vec![], None);
    let test2 =
        p.fb.call(r, e, vec![copy_local(p.pb), copy_local(p.j)], Some(y));
    p.fb.branch(test2, y, p.yes, p.no);
    pb.add(p.fb.finish());
    assert!(!rewrites(&pb.finish(), &edges(16)));
}

#[test]
fn an_array_or_index_changed_between_the_tests_stays() {
    let grid = edges(16);
    // The second test's array is set in its block, so the first block cannot read it. `pa` is
    // `pb`'s array here: still another value than `pb` held at the first test.
    let p = shaped(|p, b| {
        let other = p.fb.local(Ty::Ptr);
        p.fb.assign(b.r, other, Rvalue::Use(copy_local(p.pa)));
        second_test_of(p, b, copy_local(other));
    });
    assert!(!rewrites(&p, &grid));
    // The array's own local is set from an earlier one, which differs on purpose.
    let p = shaped(|p, b| {
        let (from, to) = (p.pa, p.pb);
        p.fb.assign(b.r, to, Rvalue::Use(copy_local(from)));
    });
    assert!(!rewrites(&p, &grid));
    // The index moves between the tests, as `fits(a, i) && fits(b, ++i)` does.
    let p = shaped(|p, b| {
        let j = p.j;
        p.fb.assign(b.r, j, bin(BinOp::Add, copy_local(j), int(1, Ty::U64)));
    });
    assert!(!rewrites(&p, &grid));
}

#[test]
fn an_address_taken_index_stays() {
    // Its value is memory now: a store through its address may change it between the tests.
    let p = shaped(|p, _| {
        let addr = p.fb.local(Ty::Ptr);
        let j = p.j;
        p.fb.assign(p.entry, addr, Rvalue::AddrOf(Place::local(j)));
    });
    assert!(!rewrites(&p, &edges(16)));
}

#[test]
fn a_second_block_with_another_way_in_stays() {
    // `r` is also where the function goes without any first test.
    let mut pb = ProgramBuilder::new();
    let agg = header(&mut pb);
    let e = fits_ext(&mut pb, Lanes::I8);
    let mut p = parts(agg, e.clone());
    let (x, y, skip) = (
        p.fb.local(Ty::Bool),
        p.fb.local(Ty::Bool),
        p.fb.local(Ty::Bool),
    );
    let first = p.fb.block();
    let r = p.fb.block();
    p.fb.assign(
        p.entry,
        skip,
        bin(BinOp::Eq, copy_local(p.j), int(7, Ty::U64)),
    );
    p.fb.branch(p.entry, skip, r, first);
    let test1 = p.fb.call(
        first,
        e.clone(),
        vec![copy_local(p.pa), copy_local(p.j)],
        Some(x),
    );
    p.fb.branch(test1, x, r, p.no);
    let test2 =
        p.fb.call(r, e, vec![copy_local(p.pb), copy_local(p.j)], Some(y));
    p.fb.branch(test2, y, p.yes, p.no);
    pb.add(p.fb.finish());
    assert!(!rewrites(&pb.finish(), &edges(16)));
}

#[test]
fn a_first_test_that_guards_a_null_array_stays() {
    // `fits(a, j) && b !== null && fits(b, j)`: a block between the tests, so `b` (null here) is
    // never read when the check fails, with the rewrite as without it.
    let mut pb = ProgramBuilder::new();
    let agg = header(&mut pb);
    let e = fits_ext(&mut pb, Lanes::I8);
    let mut p = parts(agg, e.clone());
    let (x, y, some) = (
        p.fb.local(Ty::Bool),
        p.fb.local(Ty::Bool),
        p.fb.local(Ty::Bool),
    );
    let nullable = p.fb.local(Ty::Ptr);
    p.fb.assign(p.entry, nullable, Rvalue::Use(int(0, Ty::Ptr)));
    let test1 = p.fb.call(
        p.entry,
        e.clone(),
        vec![copy_local(p.pa), copy_local(p.j)],
        Some(x),
    );
    let (check, r) = (p.fb.block(), p.fb.block());
    p.fb.branch(test1, x, check, p.no);
    p.fb.assign(
        check,
        some,
        bin(BinOp::Ne, copy_local(nullable), int(0, Ty::Ptr)),
    );
    p.fb.branch(check, some, r, p.no);
    let test2 =
        p.fb.call(r, e, vec![copy_local(nullable), copy_local(p.j)], Some(y));
    p.fb.branch(test2, y, p.yes, p.no);
    pb.add(p.fb.finish());
    assert!(!rewrites(&pb.finish(), &edges(16)));
}

#[test]
fn an_array_read_in_the_second_block_stays() {
    // `fits(a, j) && fits(*slot, j)`: the load would have to run before the first test.
    let p = shaped(|p, b| {
        let (slot, array) = (p.fb.local(Ty::Ptr), p.fb.local(Ty::Ptr));
        let (ppb, cell) = (p.pb, p.fb.local(Ty::Ptr));
        p.fb.assign(p.entry, cell, Rvalue::Use(copy_local(ppb)));
        p.fb.assign(p.entry, slot, Rvalue::AddrOf(Place::local(cell)));
        p.fb.assign(b.r, array, Rvalue::Use(copy_place(deref(slot, Ty::Ptr))));
        second_test_of(p, b, copy_local(array));
    });
    assert!(!rewrites(&p, &edges(16)));
}

#[test]
fn a_program_without_fits_is_left_alone() {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[], Ty::I64);
    let b = fb.block();
    fb.ret(b, int(1, Ty::I64));
    pb.add(fb.finish());
    let mut p = pb.finish();
    assert!(run(&mut p).is_empty());
    assert!(p.externs.is_empty());
}
