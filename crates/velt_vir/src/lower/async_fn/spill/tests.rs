//! The spill analysis on hand-built poll functions: the per-local liveness agrees with the
//! classic dense fixpoint, maybe-written over a subset of the locals is the full analysis
//! restricted to it, and a long function with one suspension costs work linear in its size.

use super::super::{DONE, PLACEHOLDER};
use super::flow::{live_in_at, maybe_written, Bits, Susp, Universe, WORDS};
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
        cold: false,
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
    use super::flow::{has_deref, stmt_acc, term_acc, Acc};
    let (n, nb) = (f.locals.len(), f.blocks.len());
    let mut gen = vec![Bits::new(n); nb];
    let mut kill = vec![Bits::new(n); nb];
    for b in 0..nb {
        let mut accs = vec![];
        f.blocks[b]
            .stmts
            .iter()
            .for_each(|s| stmt_acc(s, &mut accs));
        term_acc(&f.blocks[b].term, &mut accs);
        for acc in accs {
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

#[test]
fn maybe_written_over_a_subset_is_the_full_analysis_restricted() {
    for seed in 0..200 {
        let f = random_fn(seed + 1000, 2 + seed as usize % 19, 2 + seed as usize % 90);
        let (nb, n) = (f.blocks.len(), f.locals.len());
        let s = Susp {
            extra: vec![(seed as usize % nb, (seed as usize * 7) % nb)],
            starts: vec![1 % nb],
            inputs: vec![seed as usize % n, 1],
        };
        let mut all = Bits::new(n);
        let mut sub = Bits::new(n);
        for l in 0..n {
            all.set(l);
            if l % 3 != 1 {
                sub.set(l);
            }
        }
        let (ua, us) = (Universe::of_set(n, &all), Universe::of_set(n, &sub));
        let (wa, ws) = (maybe_written(&f, &s, &ua), maybe_written(&f, &s, &us));
        for b in 0..nb {
            for l in sub.iter() {
                let i = us.index(l).unwrap();
                assert_eq!(
                    ws[b].get(i),
                    wa[b].get(l),
                    "seed {seed} block {b} local {l}"
                );
            }
        }
    }
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

/// Words of dense sets the analysis makes for `f`, and the locals it spills.
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
    assert!(large < 64 * 4000, "{large} words for 4000 calls");
}
