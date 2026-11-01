//! Headers a loop's copy knows to be plain (view_unswitch.rs).
//!
//! A test in the copy that finds a view goes on in the original, so past it (on its false
//! side) the header it read is a plain array's. So is a header whose length is not 0 (a view's
//! own header has length 0). As long as nothing may write memory (no call but of a quiet
//! function given only numbers, as an index's conversion; no store through a pointer; no write
//! to a local whose address is taken; an `await` polls through a call, and an `await` or a
//! `yield` returns from the function) and the locals the header's place is computed from keep
//! their values, a later read of the same place reads the same header, and it is still plain
//! there:
//! - a later view test of it is false in the copy;
//! - an element access past its length is the plain array's out-of-bounds error, so the copy
//!   goes on in its own copy of the access (whose view test is false) instead of in the
//!   original. As in a program without views, the loop then has no way out at that test but
//!   the error, which lets the backend compile the loop as it would that program's.
//!
//! The places are compared as terms: what each local holds, built from the statements since
//! the loop's header (copies, arithmetic, casts and reads through pointers), forgotten when a
//! local in it is assigned or, for a read of memory, when memory may change.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_sema::hir::DefId;

use super::view_unswitch::Tests;
use super::FnLower;
use crate::vir::{self, BinOp, BlockId, Const, Local, Operand, Place, Proj, Rvalue, Terminator};
use crate::vir::{Ty, UnOp};

/// The prelude's conversion of a number index (velt_sema `numbers.rs`), which reads nothing but
/// its number.
pub(super) const FLOAT_INDEX: &str = velt_common::std_hook!(float_index);

/// A value as computed from the values the locals in it hold at the point.
#[derive(Clone, Debug, PartialEq)]
enum Term {
    Local(Local),
    Const(Const, Ty),
    /// A field of an aggregate, or (`Deref`) a read of memory.
    Proj(Box<Term>, Proj),
    Unary(UnOp, Box<Term>),
    Binary(BinOp, Box<Term>, Box<Term>),
    Cast(Box<Term>, Ty),
    /// The result of a quiet function (of its definition) given only numbers.
    Call(DefId, Vec<Term>),
}

impl Term {
    fn any(&self, f: &impl Fn(&Term) -> bool) -> bool {
        f(self)
            || match self {
                Term::Local(_) | Term::Const(..) => false,
                Term::Proj(t, _) | Term::Unary(_, t) | Term::Cast(t, _) => t.any(f),
                Term::Binary(_, a, b) => a.any(f) || b.any(f),
                Term::Call(_, ts) => ts.iter().any(|t| t.any(f)),
            }
    }

    fn mentions(&self, l: Local) -> bool {
        self.any(&|t| matches!(t, Term::Local(x) if *x == l))
    }

    /// Does the value depend on memory (a read through a pointer, or a local whose address is
    /// taken, which a store through a pointer or a call may change)?
    fn reads_memory(&self, ls: &Locals) -> bool {
        self.any(&|t| match t {
            Term::Proj(_, Proj::Deref(_)) => true,
            Term::Local(l) => ls.addressed.contains(l),
            _ => false,
        })
    }

    fn contains(&self, x: &Term) -> bool {
        self.any(&|t| t == x)
    }

    /// Is the term small enough to keep (a longer one is forgotten, which proves less but
    /// keeps the work per statement bounded)?
    fn small(&self) -> bool {
        let n = std::cell::Cell::new(0u32);
        !self.any(&|_| {
            n.set(n.get() + 1);
            n.get() > MAX_TERM
        })
    }
}

/// The most nodes a term kept for a local has.
const MAX_TERM: u32 = 48;

/// The most rounds [`FnLower::plain_headers`] takes before it proves nothing.
const MAX_ROUNDS: u32 = 8;

/// The function's locals as [`Known`] sees them.
struct Locals {
    /// Those whose own address is taken.
    addressed: FxHashSet<Local>,
    /// Those (not parameters, nor addressed) assigned in one place only: a term for one of them
    /// is its assignment's, the same on every way there.
    once: FxHashSet<Local>,
}

/// What is known at a point of the copy: the terms locals hold, and the plain headers.
#[derive(Clone, Default, PartialEq)]
struct Known {
    env: FxHashMap<Local, Term>,
    plain: Vec<Term>,
}

impl Known {
    fn meet(&mut self, other: &Known) -> bool {
        let (env, plain) = (self.env.len(), self.plain.len());
        self.env.retain(|l, t| other.env.get(l) == Some(t));
        self.plain.retain(|t| other.plain.contains(t));
        env != self.env.len() || plain != self.plain.len()
    }

    fn forget_memory(&mut self, ls: &Locals) {
        self.env.retain(|_, t| !t.reads_memory(ls));
        self.plain.retain(|t| !t.reads_memory(ls));
    }

    fn forget_local(&mut self, l: Local, ls: &Locals) {
        self.env.remove(&l);
        self.env.retain(|_, t| !t.mentions(l));
        self.plain.retain(|t| !t.mentions(l));
        if ls.addressed.contains(&l) {
            self.forget_memory(ls);
        }
    }

    /// A write to `p`.
    fn write(&mut self, p: &Place, ls: &Locals) {
        match p.proj.iter().any(|x| matches!(x, Proj::Deref(_))) {
            true => self.forget_memory(ls),
            false => self.forget_local(p.local, ls),
        }
    }

    fn place(&self, p: &Place) -> Term {
        let base = (self.env.get(&p.local).cloned()).unwrap_or(Term::Local(p.local));
        (p.proj.iter()).fold(base, |t, x| Term::Proj(Box::new(t), x.clone()))
    }

    fn operand(&self, o: &Operand) -> Term {
        match o {
            Operand::Copy(p) => self.place(p),
            Operand::Const(c, t) => Term::Const(c.clone(), *t),
        }
    }

    fn rvalue(&self, rv: &Rvalue) -> Option<Term> {
        let b = |o: &Operand| Box::new(self.operand(o));
        Some(match rv {
            Rvalue::Use(o) => self.operand(o),
            Rvalue::Unary(op, o) => Term::Unary(*op, b(o)),
            Rvalue::Binary(op, x, y) => Term::Binary(*op, b(x), b(y)),
            Rvalue::Cast(o, t) => Term::Cast(b(o), *t),
            Rvalue::AddrOf(_) | Rvalue::Aggregate(..) => return None,
        })
    }

    /// After a write to `p`: the local holds `t` (when it is assigned only there).
    fn define(&mut self, p: &Place, t: Option<Term>, ls: &Locals) {
        let keep = |t: &Term| {
            p.proj.is_empty() && ls.once.contains(&p.local) && !t.mentions(p.local) && t.small()
        };
        if let Some(t) = t.filter(keep) {
            self.env.insert(p.local, t);
        }
    }

    fn stmt(&mut self, s: &vir::Stmt, ls: &Locals) {
        match s {
            vir::Stmt::Assign(p, rv) => {
                let t = self.rvalue(rv);
                self.write(p, ls);
                self.define(p, t, ls);
            }
            vir::Stmt::MemCopy { .. } | vir::Stmt::MemCopyDyn { .. } | vir::Stmt::MemSet { .. } => {
                self.forget_memory(ls)
            }
            vir::Stmt::Nop => {}
        }
    }
}

/// What [`FnLower::plain_headers`] proved for a loop's copy: the blocks whose view test is
/// false there, and those whose element access past the length is a plain array's.
pub(super) struct Proven {
    pub(super) tests: FxHashSet<BlockId>,
    pub(super) slows: FxHashSet<BlockId>,
}

impl FnLower<'_, '_> {
    /// The tests of the copy of the loop `region` (header `h`, tests `tests`) that the module
    /// docs prove.
    pub(super) fn plain_headers(
        &self,
        h: BlockId,
        region: &[BlockId],
        tests: &Tests,
        params: usize,
    ) -> Proven {
        let mut proven = Proven {
            tests: FxHashSet::default(),
            slows: FxHashSet::default(),
        };
        if tests.leave.is_empty() && tests.slows.is_empty() {
            return proven;
        }
        let ls = self.locals_seen(params);
        let cond = |b: BlockId| match &self.blocks[b.0 as usize].1 {
            Some(Terminator::Branch {
                cond: Operand::Copy(Place { local, proj }),
                ..
            }) if proj.is_empty() => Some(*local),
            _ => None,
        };
        // Only an access whose way on past the length is in the loop (and so in its copy).
        let on_in_loop = |b: BlockId| match &self.blocks[b.0 as usize].1 {
            Some(Terminator::Branch { els, .. }) => region.contains(els),
            _ => false,
        };
        let mut slows: FxHashSet<BlockId> = (region.iter().copied())
            .filter(|&b| cond(b).is_some_and(|c| tests.slows.contains(&c)) && on_in_loop(b))
            .collect();
        // Assume every access past the length is proved (its way on stays in the copy), and
        // drop those that are not until the rest are proved with the copy they make.
        for _ in 0..MAX_ROUNDS {
            let outs = self.plain_flow(h, region, tests, &slows, &ls);
            let plain_at = |b: BlockId| {
                let c = cond(b)?;
                let k = outs.get(&b)?;
                let hdr = self.tested_header(c, k)?;
                k.plain.contains(&hdr).then_some(c)
            };
            let still: FxHashSet<BlockId> = slows
                .iter()
                .copied()
                .filter(|&b| plain_at(b).is_some())
                .collect();
            if still.len() == slows.len() {
                proven.tests = (region.iter().copied())
                    .filter(|&b| plain_at(b).is_some_and(|c| tests.leave.contains(&c)))
                    .collect();
                proven.slows = slows;
                return proven;
            }
            slows = still;
        }
        proven
    }

    /// The header the test `c` read, as a term at a point where `k` is known, when `c` still
    /// holds what it read there.
    fn tested_header(&self, c: Local, k: &Known) -> Option<Term> {
        let p = self.view_loops.hdrs.get(&c)?;
        let hdr = k.place(p);
        k.env.get(&c).filter(|t| t.contains(&hdr)).map(|_| hdr)
    }

    /// What is known at the end of each block of the copy of the loop `region` (before its
    /// terminator), with the accesses `slows` going on in the copy past the length.
    fn plain_flow(
        &self,
        h: BlockId,
        region: &[BlockId],
        tests: &Tests,
        slows: &FxHashSet<BlockId>,
        ls: &Locals,
    ) -> FxHashMap<BlockId, Known> {
        let inside: FxHashSet<BlockId> = region.iter().copied().collect();
        let mut ins: FxHashMap<BlockId, Known> = FxHashMap::default();
        let mut outs: FxHashMap<BlockId, Known> = FxHashMap::default();
        ins.insert(h, Known::default());
        let mut work = vec![h];
        while let Some(b) = work.pop() {
            let (stmts, term) = &self.blocks[b.0 as usize];
            let mut k = ins[&b].clone();
            stmts.iter().for_each(|s| k.stmt(s, ls));
            let mut edges: Vec<(BlockId, Known)> = vec![];
            match term {
                Some(Terminator::Branch { cond, then, els }) => {
                    let c = match cond {
                        Operand::Copy(Place { local, proj }) if proj.is_empty() => Some(*local),
                        _ => None,
                    };
                    let is = |set: &FxHashSet<Local>| c.is_some_and(|c| set.contains(&c));
                    // The false side of a view test or of a zero length test: plain.
                    let mut plain = k.clone();
                    let view = is(&tests.plain) || is(&tests.leave);
                    if view || is(&self.view_loops.zeros) {
                        if let Some(hdr) = c.and_then(|c| self.tested_header(c, &k)) {
                            if !plain.plain.contains(&hdr) {
                                plain.plain.push(hdr);
                            }
                        }
                    }
                    let slow = is(&tests.slows);
                    if !view {
                        edges.push((*then, k.clone()));
                    }
                    if !slow || slows.contains(&b) {
                        edges.push((*els, plain));
                    }
                }
                Some(Terminator::Call {
                    callee,
                    args,
                    dest,
                    next,
                }) => {
                    let quiet = self.quiet_call(callee, args);
                    if quiet.is_none() {
                        k.forget_memory(ls);
                    }
                    if let Some(p) = dest {
                        let t = quiet
                            .map(|d| Term::Call(d, args.iter().map(|a| k.operand(a)).collect()));
                        k.write(p, ls);
                        k.define(p, t, ls);
                    }
                    edges.push((*next, k.clone()));
                }
                Some(t) => {
                    let ss = super::cfg::successors(t);
                    edges.extend(ss.into_iter().map(|s| (s, k.clone())));
                }
                None => {}
            }
            outs.insert(b, k);
            for (s, k) in edges {
                if s == h || !inside.contains(&s) {
                    continue;
                }
                let changed = match ins.get_mut(&s) {
                    Some(old) => old.meet(&k),
                    None => {
                        ins.insert(s, k);
                        true
                    }
                };
                if changed {
                    work.push(s);
                }
            }
        }
        outs
    }

    /// Is this a call of a quiet function (quiet.rs) or of [`FLOAT_INDEX`] given only numbers,
    /// which then cannot write memory (it has nothing to write through, and module state is not
    /// quiet)? Its definition when it is: the same numbers give the same result.
    fn quiet_call(&self, callee: &vir::Callee, args: &[Operand]) -> Option<DefId> {
        let number = |t: Ty| t.is_int() || t.is_float() || t == Ty::Bool;
        let vir::Callee::Func(f) = callee else {
            return None;
        };
        let def = *self.view_loops.quiet.get(f)?;
        let numbers = args.iter().all(|a| match a {
            Operand::Const(_, t) => number(*t),
            Operand::Copy(p) => p.proj.is_empty() && number(self.locals[p.local.0 as usize].ty),
        });
        numbers.then_some(def)
    }

    /// The function's locals for [`Known`]: those whose own address is taken, and those
    /// assigned once.
    fn locals_seen(&self, params: usize) -> Locals {
        let mut addressed = FxHashSet::default();
        let mut sites: FxHashMap<Local, u32> = FxHashMap::default();
        let mut assign = |p: &Place| {
            if !matches!(p.proj.first(), Some(Proj::Deref(_))) {
                *sites.entry(p.local).or_default() += 1;
            }
        };
        for (stmts, term) in &self.blocks {
            for s in stmts {
                if let vir::Stmt::Assign(p, rv) = s {
                    assign(p);
                    if let Rvalue::AddrOf(a) = rv {
                        if !matches!(a.proj.first(), Some(Proj::Deref(_))) {
                            addressed.insert(a.local);
                        }
                    }
                }
            }
            if let Some(Terminator::Call { dest: Some(p), .. }) = term {
                assign(p);
            }
        }
        let once = (sites.into_iter())
            .filter(|&(l, n)| n == 1 && l.0 as usize >= params && !addressed.contains(&l))
            .map(|(l, _)| l)
            .collect();
        Locals { addressed, once }
    }
}
