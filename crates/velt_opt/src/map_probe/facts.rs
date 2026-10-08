//! Facts about one function that every probe comparison uses: the single definition of each
//! local defined once, which locals hold the address of which, whose address escapes, every
//! write of each local, and the code between two points (cached).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use velt_vir::vir::{AggLayout, Function, Local, Operand, Place, Rvalue, Stmt, Terminator};

use super::region::{between, predecessors, Dominators, Point, Region};
use super::{call_at, term_point, Probes};
use crate::locals::Usage;
use crate::visit::{derefs, stmt_operands, term_operands};

/// `between(q, p)` per pair of points, computed once.
type RegionCache = HashMap<(Point, Point), Option<Rc<Region>>>;

/// Facts about one function, shared by every query.
pub(super) struct Cx<'f> {
    pub(super) aggs: &'f [AggLayout],
    pub(super) func: &'f Function,
    pub(super) probes: &'f Probes,
    pub(super) preds: Vec<Vec<usize>>,
    pub(super) doms: Dominators,
    pub(super) usage: Usage,
    /// The definition of each register-like local assigned exactly once (not a param).
    pub(super) defs: Vec<Option<Point>>,
    /// The write of each aggregate local written exactly once, as a whole, and never through
    /// its address (number keys are reinterpreted through one to be hashed).
    pub(super) agg_defs: Vec<Option<Point>>,
    /// For a register-like local defined once as `&z`: `z`.
    pub(super) addr: Vec<Option<Local>>,
    /// Locals whose address is used other than as a call argument.
    pub(super) escaped: Vec<bool>,
    /// Points that write each local (directly, or through `&local` passed to a writing call).
    pub(super) writes: Vec<Vec<Point>>,
    pub(super) regions: RefCell<RegionCache>,
}

impl<'f> Cx<'f> {
    pub(super) fn new(aggs: &'f [AggLayout], func: &'f Function, probes: &'f Probes) -> Cx<'f> {
        let n = func.locals.len();
        let preds = predecessors(func);
        let mut cx = Cx {
            aggs,
            func,
            probes,
            doms: Dominators::new(func, &preds),
            preds,
            usage: Usage::of(func),
            defs: vec![None; n],
            agg_defs: vec![None; n],
            addr: vec![None; n],
            escaped: vec![false; n],
            writes: vec![vec![]; n],
            regions: RefCell::default(),
        };
        cx.scan_defs();
        cx.scan_addresses();
        cx.scan_writes();
        cx
    }

    fn scan_defs(&mut self) {
        let params = self.func.params.len();
        for (block, b) in self.func.blocks.iter().enumerate() {
            for (index, s) in b.stmts.iter().enumerate() {
                if let Stmt::Assign(place, _) = s {
                    self.note_def(place, Point { block, index }, params);
                }
            }
            if let Terminator::Call { dest: Some(d), .. } = &b.term {
                self.note_def(d, term_point(self.func, block), params);
            }
        }
    }

    fn note_def(&mut self, place: &Place, at: Point, params: usize) {
        let l = place.local;
        let u = self.usage.get(l);
        if !place.proj.is_empty() || (l.0 as usize) < params || u.defs != 1 {
            return;
        }
        if self.usage.is_register(l) {
            self.defs[l.0 as usize] = Some(at);
        } else if !u.address_taken && u.partial_defs == 0 {
            self.agg_defs[l.0 as usize] = Some(at);
        }
    }

    fn scan_addresses(&mut self) {
        for b in &self.func.blocks {
            for s in &b.stmts {
                if let Stmt::Assign(dst, Rvalue::AddrOf(p)) = s {
                    let named = dst.proj.is_empty() && self.defs[dst.local.0 as usize].is_some();
                    if named && p.proj.is_empty() {
                        self.addr[dst.local.0 as usize] = Some(p.local);
                    } else {
                        self.escaped[p.local.0 as usize] = true;
                    }
                }
            }
        }
        // An address used by anything but a call argument (copied, stored, returned) escapes.
        let mut leaked = vec![];
        let mut note = |op: &Operand| {
            if let Operand::Copy(p) = op {
                leaked.push(p.local);
            }
        };
        for b in &self.func.blocks {
            b.stmts.iter().for_each(|s| stmt_operands(s, &mut note));
            if !matches!(b.term, Terminator::Call { .. }) {
                term_operands(&b.term, &mut note);
            }
        }
        for p in leaked {
            if let Some(z) = self.addr[p.0 as usize] {
                self.escaped[z.0 as usize] = true;
            }
        }
    }

    fn scan_writes(&mut self) {
        for (block, b) in self.func.blocks.iter().enumerate() {
            for (index, s) in b.stmts.iter().enumerate() {
                let at = Point { block, index };
                let target = match s {
                    Stmt::Assign(place, _) if !derefs(place) => Some(place.local),
                    Stmt::MemCopy { dst, .. }
                    | Stmt::MemCopyDyn { dst, .. }
                    | Stmt::MemSet { dst, .. } => self.addr_local(dst),
                    _ => None,
                };
                if let Some(l) = target {
                    self.writes[l.0 as usize].push(at);
                }
            }
            if let Terminator::Call {
                callee, args, dest, ..
            } = &b.term
            {
                let at = term_point(self.func, block);
                if let Some(d) = dest.as_ref().filter(|d| !derefs(d)) {
                    self.writes[d.local.0 as usize].push(at);
                }
                if !self.probes.reads_only(callee) {
                    let written: Vec<Local> =
                        args.iter().filter_map(|a| self.addr_local(a)).collect();
                    for z in written {
                        self.writes[z.0 as usize].push(at);
                    }
                }
            }
        }
    }

    /// The local `op` is the address of (`&z`, defined once), if any.
    pub(super) fn addr_local(&self, op: &Operand) -> Option<Local> {
        match op {
            Operand::Copy(p) if p.proj.is_empty() => *self.addr.get(p.local.0 as usize)?,
            _ => None,
        }
    }

    pub(super) fn region(&self, q: Point, p: Point) -> Option<Rc<Region>> {
        self.regions
            .borrow_mut()
            .entry((q, p))
            .or_insert_with(|| between(self.func, &self.preds, &self.doms, q, p).map(Rc::new))
            .clone()
    }

    /// The local holding the result of the probe ending block `q`, if only that call writes it.
    pub(super) fn result_of(&self, q: usize) -> Option<Local> {
        let (_, _, dest) = call_at(self.func, q)?;
        let d = dest.as_ref()?;
        (d.proj.is_empty() && self.defs[d.local.0 as usize].is_some()).then_some(d.local)
    }
}
