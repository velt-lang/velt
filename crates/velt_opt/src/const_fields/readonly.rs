//! Read-only pointer parameters: a `Ptr` param that the function only dereferences to *read*
//! (never writes through, never reassigns, never lets escape) — except that it may pass the
//! pointer unchanged to another function's read-only param. Computed as a greatest fixpoint
//! over the whole program so recursive chains (`introsort` passing `less` to itself) qualify.
//!
//! A caller that hands `&a` only to read-only params knows that nothing writes `a` during
//! those calls, so facts about `a`'s memory hold inside the callees too.
//!
//! Every check scans a body once for a whole set of pointer locals (all params of a function,
//! all address-holding locals of an aggregate), so large functions with many candidates stay
//! linear (checking each candidate with its own scan was quadratic in `main`-sized bodies).

use velt_vir::vir::{
    Callee, Function, Local, Operand, Place, Program, Proj, Rvalue, Stmt, Terminator, Ty,
};

use crate::visit::{stmt_operands, term_operands};

/// `params[f][i]`: whether param `i` of function `f` is read-only.
pub(crate) struct ReadOnly {
    params: Vec<Vec<bool>>,
}

impl ReadOnly {
    /// Analyze the whole program.
    pub fn of(program: &Program) -> ReadOnly {
        let mut ro = ReadOnly {
            params: program
                .funcs
                .iter()
                .map(|f| f.params.iter().map(|&t| t == Ty::Ptr).collect())
                .collect(),
        };
        // A function's verdicts only depend on its callees' params, so after a change only its
        // callers are checked again (a worklist rather than rounds over the whole program).
        let callers = callers(program);
        let mut queued = vec![true; program.funcs.len()];
        let mut work: Vec<usize> = (0..program.funcs.len()).rev().collect();
        while let Some(fi) = work.pop() {
            queued[fi] = false;
            let func = &program.funcs[fi];
            let candidates: Vec<Local> = (0..func.params.len())
                .filter(|&pi| ro.params[fi][pi])
                .map(|pi| Local(pi as u32))
                .collect();
            if candidates.is_empty() {
                continue;
            }
            let verdicts = ro.read_only_locals(func, &candidates, false);
            let mut changed = false;
            for (p, ok) in candidates.iter().zip(verdicts) {
                if !ok {
                    ro.params[fi][p.0 as usize] = false;
                    changed = true;
                }
            }
            if changed {
                for &c in &callers[fi] {
                    if !std::mem::replace(&mut queued[c], true) {
                        work.push(c);
                    }
                }
            }
        }
        ro
    }

    /// Whether param `index` of function `func` is read-only.
    pub fn get(&self, func: usize, index: usize) -> bool {
        self.params
            .get(func)
            .and_then(|ps| ps.get(index))
            .copied()
            .unwrap_or(false)
    }

    /// Whether `op`, passed as argument `index` to `callee`, stays read-only there.
    pub fn arg_is_read_only(&self, callee: &Callee, index: usize) -> bool {
        matches!(callee, Callee::Func(id) if self.get(id.0 as usize, index))
    }

    /// Per candidate pointer local of `func`: whether every use of it only reads through it
    /// (or passes it to read-only params). With `own_def`, whole assignments `p = …` are
    /// allowed (the caller checks that `p` has the single definition it expects).
    pub fn read_only_locals(
        &self,
        func: &Function,
        candidates: &[Local],
        own_def: bool,
    ) -> Vec<bool> {
        let mut scan = Scan::new(func.locals.len(), candidates);
        for b in &func.blocks {
            for s in &b.stmts {
                scan.stmt(s, own_def);
            }
            scan.term(self, &b.term);
        }
        candidates.iter().map(|&p| !scan.failed(p)).collect()
    }
}

/// One pass over a body, recording which candidate locals were used in a non-read-only way.
struct Scan {
    /// Per local: `Some(false)` for a candidate still read-only, `Some(true)` once it fails.
    state: Vec<Option<bool>>,
}

impl Scan {
    fn new(locals: usize, candidates: &[Local]) -> Scan {
        let mut state = vec![None; locals];
        for p in candidates {
            if let Some(s) = state.get_mut(p.0 as usize) {
                *s = Some(false);
            }
        }
        Scan { state }
    }

    fn is_candidate(&self, l: Local) -> bool {
        self.state.get(l.0 as usize).is_some_and(|s| s.is_some())
    }

    fn fail(&mut self, l: Local) {
        if let Some(s @ Some(_)) = self.state.get_mut(l.0 as usize) {
            *s = Some(true);
        }
    }

    fn failed(&self, l: Local) -> bool {
        self.state.get(l.0 as usize) != Some(&Some(false))
    }

    /// An operand mentioning a candidate must only read memory through it.
    fn read(&mut self, op: &Operand) {
        if let Operand::Copy(place) = op {
            if self.is_candidate(place.local) && !derefs_first(place) {
                self.fail(place.local);
            }
        }
    }

    fn stmt(&mut self, s: &Stmt, own_def: bool) {
        stmt_operands(s, &mut |op| self.read(op));
        match s {
            Stmt::Assign(dst, rv) => {
                if let Rvalue::AddrOf(q) = rv {
                    self.fail(q.local);
                }
                if !(own_def && dst.proj.is_empty()) {
                    self.fail(dst.local);
                }
            }
            // The destination pointers of memory statements are writes.
            Stmt::MemCopy { dst, .. } | Stmt::MemCopyDyn { dst, .. } | Stmt::MemSet { dst, .. } => {
                if let Operand::Copy(q) = dst {
                    self.fail(q.local);
                }
            }
            Stmt::Nop => {}
        }
    }

    fn term(&mut self, ro: &ReadOnly, t: &Terminator) {
        let Terminator::Call {
            callee, args, dest, ..
        } = t
        else {
            term_operands(t, &mut |op| self.read(op));
            return;
        };
        if let Some(d) = dest {
            self.fail(d.local);
        }
        for (i, a) in args.iter().enumerate() {
            if let Operand::Copy(place) = a {
                if place.proj.is_empty() && !ro.arg_is_read_only(callee, i) {
                    self.fail(place.local);
                }
            }
        }
        if let Callee::Ptr { target, .. } = callee {
            self.read(target);
        }
    }
}

/// Per function: the functions that call it directly (with duplicates).
fn callers(program: &Program) -> Vec<Vec<usize>> {
    let mut out = vec![vec![]; program.funcs.len()];
    for (fi, func) in program.funcs.iter().enumerate() {
        for b in &func.blocks {
            if let Terminator::Call {
                callee: Callee::Func(id),
                ..
            } = &b.term
            {
                if let Some(list) = out.get_mut(id.0 as usize) {
                    list.push(fi);
                }
            }
        }
    }
    out
}

/// `op` is exactly the value of local `p`.
pub(crate) fn is_bare(op: &Operand, p: Local) -> bool {
    matches!(op, Operand::Copy(place) if place.local == p && place.proj.is_empty())
}

/// The place starts by dereferencing its local (so the local is used as a pointer only).
pub(crate) fn derefs_first(place: &Place) -> bool {
    matches!(place.proj.first(), Some(Proj::Deref(_)))
}
