//! The facts view_unswitch.rs reads from the whole function before versioning its loops, each
//! in one pass over the function's statements: the blocks using each view test, and the
//! locals whose address is taken with the locals holding it.
//!
//! Both were once asked per test or per address-taken local, each question a walk of the
//! whole function (and the holders' walk again until no copy was added): a function with
//! thousands of tests or addresses and tens of thousands of blocks (a large generator's poll
//! function) then took minutes to lower.

use rustc_hash::{FxHashMap, FxHashSet};

use super::FnLower;
use crate::vir::{self, BlockId, Local, Operand, Proj, Rvalue, Terminator};

/// A function's blocks while it is lowered.
type Blocks = [(Vec<vir::Stmt>, Option<Terminator>)];

#[cfg(test)]
thread_local! {
    /// Statements visited by the analyses here, which tests bound.
    static VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn visit(_n: usize) {
    #[cfg(test)]
    VISITS.with(|v| v.set(v.get() + _n));
}

impl FnLower<'_, '_> {
    /// The blocks using each local of `tested`: assigning it as a whole, or branching on it.
    pub(super) fn test_uses(
        &self,
        tested: &FxHashSet<Local>,
    ) -> FxHashMap<Local, FxHashSet<BlockId>> {
        test_uses(&self.blocks, tested)
    }

    /// The locals whose own address is taken, each with the locals holding that address
    /// (through whole-local copies), or `None` when the address escapes: any other use of a
    /// holder than a whole-local copy or an access through it (a store into a projected place,
    /// an aggregate, cast or arithmetic operand, a call argument, a return, a derived address)
    /// may let code the loop calls reach the local.
    pub(super) fn addresses(&self) -> FxHashMap<Local, Option<Vec<Local>>> {
        addresses(&self.blocks)
    }
}

fn test_uses(blocks: &Blocks, tested: &FxHashSet<Local>) -> FxHashMap<Local, FxHashSet<BlockId>> {
    let mut uses: FxHashMap<Local, FxHashSet<BlockId>> = FxHashMap::default();
    for (i, (stmts, term)) in blocks.iter().enumerate() {
        visit(stmts.len() + 1);
        let b = BlockId(i as u32);
        for s in stmts {
            if let vir::Stmt::Assign(p, _) = s {
                if p.proj.is_empty() && tested.contains(&p.local) {
                    uses.entry(p.local).or_default().insert(b);
                }
            }
        }
        if let Some(Terminator::Branch {
            cond: Operand::Copy(p),
            ..
        }) = term
        {
            if p.proj.is_empty() && tested.contains(&p.local) {
                uses.entry(p.local).or_default().insert(b);
            }
        }
    }
    uses
}

fn addresses(blocks: &Blocks) -> FxHashMap<Local, Option<Vec<Local>>> {
    let mut out: FxHashMap<Local, Option<Vec<Local>>> = FxHashMap::default();
    for (stmts, _) in blocks {
        visit(stmts.len());
        for s in stmts {
            let vir::Stmt::Assign(d, Rvalue::AddrOf(p)) = s else {
                continue;
            };
            if matches!(p.proj.first(), Some(Proj::Deref(_))) {
                continue;
            }
            let held = out.entry(p.local).or_insert_with(|| Some(vec![]));
            match (d.proj.is_empty(), held) {
                (true, Some(v)) => v.push(d.local),
                (_, held) => *held = None,
            }
        }
    }
    if out.values().all(Option::is_none) {
        return out;
    }
    let uses = HolderUses::of(blocks);
    for held in out.values_mut() {
        if let Some(v) = held {
            if uses.escapes(v) {
                *held = None;
            }
        }
    }
    out
}

/// How each local is used as a whole operand, for following the holders of an address.
struct HolderUses {
    /// Per local: the locals a whole-local copy of it is stored into.
    copies: FxHashMap<Local, Vec<Local>>,
    /// The locals used in a way that lets their value (an address) escape.
    escapes: FxHashSet<Local>,
}

impl HolderUses {
    fn of(blocks: &Blocks) -> Self {
        let mut u = HolderUses {
            copies: FxHashMap::default(),
            escapes: FxHashSet::default(),
        };
        for (stmts, term) in blocks {
            visit(stmts.len() + 1);
            for s in stmts {
                let vir::Stmt::Assign(d, rv) = s else {
                    continue;
                };
                match rv {
                    Rvalue::Use(o) => {
                        if let Some(l) = whole(o) {
                            if d.proj.is_empty() {
                                u.copies.entry(l).or_default().push(d.local);
                            } else {
                                u.escapes.insert(l);
                            }
                        }
                    }
                    Rvalue::AddrOf(p) => {
                        u.escapes.insert(p.local);
                    }
                    Rvalue::Unary(_, o) | Rvalue::Cast(o, _) => u.escape(o),
                    Rvalue::Binary(_, a, b) => {
                        u.escape(a);
                        u.escape(b);
                    }
                    Rvalue::Aggregate(_, os) => os.iter().for_each(|o| u.escape(o)),
                    Rvalue::Intrinsic(..) => {}
                }
            }
            match term {
                Some(Terminator::Call { callee, args, .. }) => {
                    if let vir::Callee::Ptr { target, .. } = callee {
                        u.escape(target);
                    }
                    args.iter().for_each(|o| u.escape(o));
                }
                Some(Terminator::Return(o)) => u.escape(o),
                _ => {}
            }
        }
        u
    }

    fn escape(&mut self, o: &Operand) {
        if let Some(l) = whole(o) {
            self.escapes.insert(l);
        }
    }

    /// Follow the whole-local copies of the holders in `v` (adding the copies to it), and
    /// tell whether any holder escapes.
    fn escapes(&self, v: &mut Vec<Local>) -> bool {
        let mut seen: FxHashSet<Local> = v.iter().copied().collect();
        let mut i = 0;
        while i < v.len() {
            let h = v[i];
            i += 1;
            if self.escapes.contains(&h) {
                return true;
            }
            for &d in self.copies.get(&h).into_iter().flatten() {
                if seen.insert(d) {
                    v.push(d);
                }
            }
        }
        false
    }
}

/// The local `o` copies as a whole.
fn whole(o: &Operand) -> Option<Local> {
    match o {
        Operand::Copy(p) if p.proj.is_empty() => Some(p.local),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vir::{Callee, Const, FuncId, Place, Stmt, Ty};

    fn place(l: u32) -> Place {
        Place {
            local: Local(l),
            proj: vec![],
        }
    }

    fn assign(d: u32, rv: Rvalue) -> Stmt {
        Stmt::Assign(place(d), rv)
    }

    fn addr(d: u32, l: u32) -> Stmt {
        assign(d, Rvalue::AddrOf(place(l)))
    }

    fn copy(d: u32, s: u32) -> Stmt {
        assign(d, Rvalue::Use(Operand::Copy(place(s))))
    }

    fn call(args: Vec<u32>) -> Option<Terminator> {
        Some(Terminator::Call {
            callee: Callee::Func(FuncId(0)),
            args: args.into_iter().map(|l| Operand::Copy(place(l))).collect(),
            dest: None,
            next: BlockId(0),
        })
    }

    fn sorted(v: &Option<Vec<Local>>) -> Option<Vec<u32>> {
        v.as_ref().map(|v| {
            let mut v: Vec<u32> = v.iter().map(|l| l.0).collect();
            v.sort();
            v
        })
    }

    #[test]
    fn holders_follow_copies_and_escapes_end_them() {
        // _1 = &_0; _2 = _1; _3 = _2 (kept). _5 = &_4; _6 = _5; call(_6) (escapes).
        // _8 = &_7; _9.f = _8 (stored into a projected place: escapes). _11 = &_10, _12 = &_11
        // (the holder's own address is taken: escapes).
        let mut field = place(9);
        field.proj.push(Proj::Field(0));
        let blocks = vec![
            (vec![copy(3, 2), addr(1, 0), copy(2, 1)], None),
            (vec![addr(5, 4), copy(6, 5)], call(vec![6])),
            (
                vec![
                    addr(8, 7),
                    Stmt::Assign(field, Rvalue::Use(Operand::Copy(place(8)))),
                ],
                Some(Terminator::Unreachable),
            ),
            (vec![addr(11, 10), addr(12, 11)], None),
        ];
        let a = addresses(&blocks);
        assert_eq!(sorted(&a[&Local(0)]), Some(vec![1, 2, 3]));
        assert_eq!(sorted(&a[&Local(4)]), None);
        assert_eq!(sorted(&a[&Local(7)]), None);
        assert_eq!(sorted(&a[&Local(10)]), None);
    }

    #[test]
    fn test_uses_finds_assignments_and_branches() {
        let blocks = vec![
            (
                vec![assign(
                    0,
                    Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)),
                )],
                None,
            ),
            (
                vec![],
                Some(Terminator::Branch {
                    cond: Operand::Copy(place(0)),
                    then: BlockId(0),
                    els: BlockId(1),
                }),
            ),
            (vec![copy(2, 1)], None),
        ];
        let tested: FxHashSet<Local> = [Local(0), Local(1)].into_iter().collect();
        let uses = test_uses(&blocks, &tested);
        let mut b0: Vec<u32> = uses[&Local(0)].iter().map(|b| b.0).collect();
        b0.sort();
        assert_eq!(b0, [0, 1]);
        assert!(!uses.contains_key(&Local(1)), "reading a test is not a use");
    }

    /// A function of `n` blocks, each taking the address of a local of its own into a holder
    /// that is copied once and passed to a call.
    fn many_addresses(n: u32) -> Vec<(Vec<Stmt>, Option<Terminator>)> {
        (0..n)
            .map(|i| {
                let (l, h, c) = (3 * i, 3 * i + 1, 3 * i + 2);
                (vec![addr(h, l), copy(c, h)], call(vec![]))
            })
            .collect()
    }

    fn visits(f: impl FnOnce()) -> usize {
        VISITS.with(|v| v.set(0));
        f();
        VISITS.with(|v| v.get())
    }

    #[test]
    fn the_analyses_visit_each_statement_a_bounded_number_of_times() {
        let small = many_addresses(1000);
        let large = many_addresses(4000);
        let tested: FxHashSet<Local> = (0..4000).map(|i| Local(3 * i + 2)).collect();
        let (s, l) = (
            visits(|| drop(addresses(&small))),
            visits(|| drop(addresses(&large))),
        );
        // Four times the code: linear work grows 4×, addresses × statements 16×.
        assert!(l <= 5 * s, "addresses: work grew {l} / {s} for 4× the code");
        assert!(l <= 4 * 4000 * 3, "addresses: {l} visits for 4000 blocks");
        let a = addresses(&large);
        assert_eq!(sorted(&a[&Local(0)]), Some(vec![1, 2]));
        let t = visits(|| drop(test_uses(&large, &tested)));
        assert!(t <= 4000 * 3, "test uses: {t} visits for 4000 blocks");
    }
}
