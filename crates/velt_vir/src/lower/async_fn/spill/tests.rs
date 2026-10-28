//! The spill analysis on hand-built poll functions: the per-local liveness agrees with the
//! classic dense fixpoint, the whole analysis (spilled locals and interference) agrees with the
//! dense formulation on generated poll functions, and long functions with one or many
//! suspensions cost work linear in their size.

use super::super::{DONE, DROP_BIT, PLACEHOLDER};
use super::flow::{has_deref, live_in_at, Acc, Bits, FORCE, WORDS};
use crate::lower::successors;
use crate::vir::{
    BasicBlock, BlockId, Callee, Const, FuncId, Function, Linkage, Local, LocalDecl, Operand,
    Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

fn func(locals: usize, blocks: Vec<BasicBlock>) -> Function {
    let decl = |ty| LocalDecl {
        ty,
        name: None,
        debug: None,
    };
    Function {
        symbol: "t$poll".into(),
        params: vec![Ty::Ptr, Ty::Ptr],
        ret: Ty::U32,
        locals: (0..locals)
            .map(|i| decl(if i < 2 { Ty::Ptr } else { Ty::I64 }))
            .collect(),
        blocks,
        linkage: Linkage::Internal,
        locs: vec![],
        param_attrs: vec![],
        is_poll: false,
        target: None,
    }
}

fn local(l: usize) -> Place {
    Place {
        local: Local(l as u32),
        proj: vec![],
    }
}

fn int(v: i128, ty: Ty) -> Operand {
    Operand::Const(Const::Int(v), ty)
}

fn copy(l: usize) -> Operand {
    Operand::Copy(local(l))
}

fn tag() -> Place {
    Place {
        local: Local(0),
        proj: vec![Proj::Deref(Ty::Agg(PLACEHOLDER)), Proj::Field(0)],
    }
}

fn block(stmts: Vec<Stmt>, term: Terminator) -> BasicBlock {
    BasicBlock { stmts, term }
}

/// Live-in sets by the textbook fixpoint over gen/kill sets of all locals.
fn dense_liveness(f: &Function) -> Vec<Vec<usize>> {
    let (n, nb) = (f.locals.len(), f.blocks.len());
    let mut gen = vec![Bits::new(n); nb];
    let mut kill = vec![Bits::new(n); nb];
    for b in 0..nb {
        for acc in super::flow::block_acc(f, b) {
            let (p, uses, kills) = match acc {
                Acc::Read(p) | Acc::Addr(p) => (p, true, false),
                Acc::Write(p) if has_deref(p) => (p, true, false),
                Acc::Write(p) => (p, false, p.proj.is_empty()),
            };
            let l = p.local.0 as usize;
            if kills {
                kill[b].set(l);
            } else if uses && !kill[b].get(l) {
                gen[b].set(l);
            }
        }
    }
    let mut live = gen.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..nb).rev() {
            let mut out = Bits::new(n);
            for s in successors(&f.blocks[b].term) {
                out.union(&live[s.0 as usize]);
            }
            out.minus(&kill[b]);
            out.union(&gen[b]);
            if out != live[b] {
                live[b] = out;
                changed = true;
            }
        }
    }
    live.iter().map(|s| s.iter().collect()).collect()
}

/// A deterministic pseudo-random function: whole, partial and through-pointer writes, reads,
/// address-taking, branches (loops included), returns and unreachable blocks.
fn random_fn(seed: u64, nb: usize, n: usize) -> Function {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut rnd = move |m: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % m as u64) as usize
    };
    let blocks = (0..nb)
        .map(|_| {
            let stmts = (0..rnd(5))
                .map(|_| {
                    let (d, s) = (rnd(n), rnd(n));
                    let dst = match rnd(4) {
                        0 => Place {
                            local: Local(d as u32),
                            proj: vec![Proj::Field(0)],
                        },
                        1 => Place {
                            local: Local(d as u32),
                            proj: vec![Proj::Deref(Ty::I64)],
                        },
                        _ => local(d),
                    };
                    let rv = match rnd(3) {
                        0 => Rvalue::AddrOf(local(s)),
                        1 => Rvalue::Use(int(1, Ty::I64)),
                        _ => Rvalue::Use(copy(s)),
                    };
                    Stmt::Assign(dst, rv)
                })
                .collect();
            let term = match rnd(6) {
                0 => Terminator::Return(copy(rnd(n))),
                1 => Terminator::Unreachable,
                2 | 3 => Terminator::Branch {
                    cond: copy(rnd(n)),
                    then: BlockId(rnd(nb) as u32),
                    els: BlockId(rnd(nb) as u32),
                },
                _ => Terminator::Goto(BlockId(rnd(nb) as u32)),
            };
            block(stmts, term)
        })
        .collect();
    func(n, blocks)
}

#[test]
fn per_local_liveness_matches_the_dense_fixpoint() {
    for seed in 0..300 {
        let f = random_fn(seed, 1 + seed as usize % 23, 2 + seed as usize % 70);
        let all: Vec<usize> = (0..f.blocks.len()).collect();
        assert_eq!(live_in_at(&f, &all), dense_liveness(&f), "seed {seed}");
    }
}

#[test]
fn liveness_is_reported_only_for_the_queried_blocks() {
    let f = random_fn(7, 20, 30);
    let full = dense_liveness(&f);
    let got = live_in_at(&f, &[3, 11]);
    for (b, live) in got.iter().enumerate() {
        let want = if b == 3 || b == 11 { &full[b][..] } else { &[] };
        assert_eq!(live, want, "block {b}");
    }
}

/// [`random_fn`] made a poll function: block 0 dispatches on the tag to the start block 1,
/// and per suspension `k` to a resume and a cancel block; `k` random blocks suspend (store the
/// tag, return). Two locals are inputs.
fn random_poll(seed: u64) -> (Function, Vec<Local>) {
    let nb = 4 + seed as usize % 21;
    let mut f = random_fn(seed + 5000, nb, 3 + seed as usize % 60);
    let mut cases = vec![(0, BlockId(1))];
    let k = 1 + seed as usize % 3;
    for j in 1..=k {
        let pick = |m: usize| 1 + (seed as usize * 31 + j * m) % (nb - 1);
        let from = pick(7);
        let blk = &mut f.blocks[from];
        blk.stmts
            .push(Stmt::Assign(tag(), Rvalue::Use(int(j as i128, Ty::U32))));
        blk.term = Terminator::Return(int(0, Ty::U32));
        cases.push((j as i128, BlockId(pick(13) as u32)));
        cases.push((j as i128 | DROP_BIT, BlockId(pick(17) as u32)));
    }
    f.blocks[0] = block(
        vec![],
        Terminator::Switch {
            value: Operand::Copy(tag()),
            cases,
            default: BlockId(nb as u32 - 1),
        },
    );
    let n = f.locals.len();
    let inputs = vec![Local(2), Local((2 + seed as usize % (n - 2)) as u32)];
    (f, inputs)
}

/// Spilled locals and interfering pairs (both orders, and a local with itself where it
/// occupies its storage) with the per-local analyses forced on or off.
fn spill_result(
    f: &Function,
    inputs: &[Local],
    per_local: bool,
) -> (Vec<usize>, Vec<(usize, usize)>) {
    FORCE.with(|c| c.set(Some(per_local)));
    let (spilled, interf) = super::analyse(f, inputs);
    FORCE.with(|c| c.set(None));
    let spilled: Vec<usize> = spilled.iter().collect();
    let mut pairs = vec![];
    for &a in &spilled {
        for &b in &spilled {
            if interf.conflict(a, b) {
                pairs.push((a, b));
            }
        }
    }
    (spilled, pairs)
}

/// The suspended locals of the module docs' step 1, by dense liveness and maybe-written.
fn dense_needed(f: &Function, inputs: &[Local]) -> Vec<usize> {
    let n = f.locals.len();
    let s = super::suspension(f, inputs);
    let live = dense_liveness(f);
    let mut all = Bits::new(n);
    (0..n).for_each(|l| all.set(l));
    let written = super::dense::maybe_written(f, &s, &super::flow::Universe::of_set(n, &all));
    let mut out: Vec<usize> = vec![];
    for &(from, to) in &s.extra {
        out.extend(live[to].iter().filter(|&&l| written[from].get(l)));
    }
    for &b in &s.starts {
        out.extend(live[b].iter().filter(|l| s.inputs.contains(l)));
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[test]
fn per_local_and_dense_analyses_agree() {
    let mut spilled = 0;
    for seed in 0..400 {
        let (f, inputs) = random_poll(seed);
        let local = spill_result(&f, &inputs, true);
        let dense = spill_result(&f, &inputs, false);
        assert_eq!(local, dense, "seed {seed}");
        let pts = super::pts::points_to(&f);
        let mut needed = Bits::new(f.locals.len());
        dense_needed(&f, &inputs)
            .into_iter()
            .for_each(|l| needed.set(l));
        let want: Vec<usize> = super::close_over_pointers(needed, &pts).iter().collect();
        assert_eq!(local.0, want, "seed {seed}: spilled");
        spilled += want.len();
    }
    // The generated functions do spill (the comparison is not of empty results).
    assert!(spilled > 400, "{spilled} locals spilled in all");
}

/// `async function main() { x = …; p = &y; f(t1) … f(tN); await …; use(x, *p) }` as a poll
/// function: block 0 dispatches on the tag, N call blocks with a fresh temporary each, the
/// suspending block, and the resume block.
fn long_poll(n: usize) -> Function {
    // Locals: 0 state, 1 cx, 2 x, 3 p, 4 y, 5 z, then t_i = 6 + 2i and its result 7 + 2i.
    let locals = 6 + 2 * n;
    let (x, p, y, z) = (2, 3, 4, 5);
    let resume = n + 3;
    let mut blocks = vec![block(
        vec![],
        Terminator::Switch {
            value: Operand::Copy(tag()),
            cases: vec![(0, BlockId(1)), (1, BlockId(resume as u32))],
            default: BlockId(resume as u32 + 1),
        },
    )];
    blocks.push(block(
        vec![
            Stmt::Assign(local(x), Rvalue::Use(int(5, Ty::I64))),
            Stmt::Assign(local(p), Rvalue::AddrOf(local(y))),
        ],
        Terminator::Goto(BlockId(2)),
    ));
    for i in 0..n {
        let t = 6 + 2 * i;
        blocks.push(block(
            vec![Stmt::Assign(local(t), Rvalue::Use(int(i as i128, Ty::I64)))],
            Terminator::Call {
                callee: Callee::Func(FuncId(0)),
                args: vec![copy(t), copy(1)],
                dest: Some(local(t + 1)),
                next: BlockId(3 + i as u32),
            },
        ));
    }
    blocks.push(block(
        vec![Stmt::Assign(tag(), Rvalue::Use(int(1, Ty::U32)))],
        Terminator::Return(int(0, Ty::U32)),
    ));
    assert_eq!(blocks.len(), resume);
    let deref_p = Place {
        local: Local(p as u32),
        proj: vec![Proj::Deref(Ty::I64)],
    };
    blocks.push(block(
        vec![
            Stmt::Assign(local(z), Rvalue::Use(Operand::Copy(deref_p))),
            Stmt::Assign(tag(), Rvalue::Use(int(DONE, Ty::U32))),
        ],
        Terminator::Return(copy(x)),
    ));
    blocks.push(block(vec![], Terminator::Unreachable));
    func(locals, blocks)
}

/// Work the analysis does for `f` (words of dense sets, walk and sweep steps), and the locals
/// it spills.
fn analysis_work(f: &Function) -> (usize, Vec<usize>) {
    WORDS.with(|w| w.set(0));
    let (spilled, _) = super::analyse(f, &[]);
    (WORDS.with(|w| w.get()), spilled.iter().collect())
}

#[test]
fn a_long_function_with_one_await_costs_linear_work() {
    let (small, s1) = analysis_work(&long_poll(1000));
    let (large, s4) = analysis_work(&long_poll(4000));
    // x (read after the await), p (the pointer read after it) and y (p's pointee).
    assert_eq!(s1, [2, 3, 4]);
    assert_eq!(s4, [2, 3, 4]);
    // Four times the code: linear work grows about 4×, blocks × locals 16×.
    assert!(
        large <= 5 * small,
        "work grew {large} / {small} for 4× the code"
    );
    assert!(large < 128 * 4000, "{large} units of work for 4000 calls");
}

/// `async function main() { for k in 1..=N { x = …; c = child(); await c; use(x) } }` as a
/// poll function: per await `k` a value `x` held across it, the child state `c`, the pointer
/// `p = &c` it is polled through, and resume and cancel blocks (the cancel block reads the
/// child through `p`). With `held`, every `x` is read only at the end, and the cancel block
/// of await `k` reads all the `x` so far (as the drops of a cancel path do): the function is
/// quadratic in N, and so is the interference (all the `x` interfere).
fn many_awaits(n: usize, held: bool) -> Function {
    // Locals: 0 state, 1 cx, 2 scratch, then per await x, c, p, r at 3 + 4(k - 1).
    let locals = 3 + 4 * n;
    // Blocks: 0 dispatch, then per await A (set up), P (poll), T (test), S (suspend),
    // R (resume), C (cancel), N (after) at 1 + 7(k - 1); the last block finishes.
    let at = |k: usize, i: usize| BlockId((1 + 7 * (k - 1) + i) as u32);
    let done = BlockId((1 + 7 * n) as u32);
    let mut cases = vec![(0, at(1, 0))];
    let mut blocks = vec![block(vec![], Terminator::Unreachable)];
    for k in 1..=n {
        let (x, c, p, r) = (
            3 + 4 * (k - 1),
            4 + 4 * (k - 1),
            5 + 4 * (k - 1),
            6 + 4 * (k - 1),
        );
        let deref_p = Place {
            local: Local(p as u32),
            proj: vec![Proj::Deref(Ty::I64)],
        };
        let next = if k == n { done } else { at(k + 1, 0) };
        blocks.extend([
            block(
                vec![
                    Stmt::Assign(local(x), Rvalue::Use(int(k as i128, Ty::I64))),
                    Stmt::Assign(local(c), Rvalue::Use(int(0, Ty::I64))),
                ],
                Terminator::Goto(at(k, 1)),
            ),
            block(
                vec![Stmt::Assign(local(p), Rvalue::AddrOf(local(c)))],
                Terminator::Call {
                    callee: Callee::Func(FuncId(0)),
                    args: vec![copy(p), copy(1)],
                    dest: Some(local(r)),
                    next: at(k, 2),
                },
            ),
            block(
                vec![],
                Terminator::Branch {
                    cond: copy(r),
                    then: at(k, 3),
                    els: at(k, 6),
                },
            ),
            block(
                vec![Stmt::Assign(tag(), Rvalue::Use(int(k as i128, Ty::U32)))],
                Terminator::Return(int(0, Ty::U32)),
            ),
            block(vec![], Terminator::Goto(at(k, 1))),
            block(
                std::iter::once(Stmt::Assign(local(2), Rvalue::Use(Operand::Copy(deref_p))))
                    .chain(
                        (1..=k)
                            .filter(|_| held)
                            .map(|j| Stmt::Assign(local(2), Rvalue::Use(copy(3 + 4 * (j - 1))))),
                    )
                    .collect(),
                Terminator::Return(int(0, Ty::U32)),
            ),
            block(
                if held {
                    vec![]
                } else {
                    vec![Stmt::Assign(local(2), Rvalue::Use(copy(x)))]
                },
                Terminator::Goto(next),
            ),
        ]);
        cases.push((k as i128, at(k, 4)));
        cases.push((k as i128 | DROP_BIT, at(k, 5)));
    }
    let reads = (1..=n)
        .filter(|_| held)
        .map(|j| Stmt::Assign(local(2), Rvalue::Use(copy(3 + 4 * (j - 1)))));
    blocks.push(block(
        reads
            .chain([Stmt::Assign(tag(), Rvalue::Use(int(DONE, Ty::U32)))])
            .collect(),
        Terminator::Return(int(0, Ty::U32)),
    ));
    blocks[0].term = Terminator::Switch {
        value: Operand::Copy(tag()),
        cases,
        default: done,
    };
    func(locals, blocks)
}

#[test]
fn a_function_with_many_awaits_costs_linear_work() {
    let (small, s1) = analysis_work(&many_awaits(250, false));
    let (large, s4) = analysis_work(&many_awaits(1000, false));
    // Per await: x (read after it), p (read by the cancel block) and c (p's pointee).
    assert_eq!(s1.len(), 3 * 250);
    assert_eq!(s4.len(), 3 * 1000);
    // Four times the awaits: linear work grows about 4×, awaits × spilled locals 16×.
    assert!(
        large <= 5 * small,
        "work grew {large} / {small} for 4× the awaits"
    );
}

#[test]
fn many_awaits_with_held_values_cost_the_dense_bound() {
    let (small, s1) = analysis_work(&many_awaits(100, true));
    let (large, s2) = analysis_work(&many_awaits(200, true));
    assert_eq!(s1.len(), 3 * 100);
    assert_eq!(s2.len(), 3 * 200);
    // Twice the awaits: the code and the interference grow 4×, dense sets over it about 8×
    // (code × spilled locals / 64); pairing every held value again at every instruction would
    // be 16×.
    assert!(
        large <= 8 * small,
        "work grew {large} / {small} for 2× the awaits"
    );
}
