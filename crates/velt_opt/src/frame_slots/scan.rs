//! Which bytes of the frame each place names, and how the whole poll function uses the frame
//! (the facts `mod.rs` chooses promoted slots from).

use std::collections::HashMap;

use velt_vir::vir::{
    AggId, AggLayout, Callee, Function, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

use crate::locals::Usage;

/// Byte range `[start, end)` of the frame.
pub(super) type Range = (u32, u32);

pub(super) fn overlaps(a: Range, b: Range) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// What a place rooted at the frame accesses.
#[derive(Debug, PartialEq)]
pub(super) enum Target {
    /// A scalar slot of the frame aggregate, named field by field. The first `end`
    /// projections of the place reach it; any further ones go through its pointer value.
    Slot {
        path: Vec<u32>,
        ty: Ty,
        range: Range,
        end: usize,
    },
    /// Frame bytes accessed another way: a whole aggregate or a reinterpreted view.
    Other(Range),
}

/// The frame of one poll function: its param, its aggregate, and the pointer locals that
/// always hold the address of one of its parts.
pub(super) struct Frame<'a> {
    pub aggs: &'a [AggLayout],
    pub param: Local,
    pub state: AggId,
    /// `q` → path, for locals assigned once with `q = &(*frame as State).path`.
    pub derived: HashMap<Local, Vec<u32>>,
}

impl Frame<'_> {
    /// Whether `l` is the frame param or a pointer derived from it.
    pub fn is_root(&self, l: Local) -> bool {
        l == self.param || self.derived.contains_key(&l)
    }

    /// Type and offset of the part at `path`.
    fn part(&self, path: &[u32]) -> Option<(Ty, u32)> {
        let (mut ty, mut off) = (Ty::Agg(self.state), 0);
        for &n in path {
            let Ty::Agg(a) = ty else { return None };
            let (t, o) = *self.aggs.get(a.0 as usize)?.fields.get(n as usize)?;
            ty = t;
            off += o;
        }
        Some((ty, off))
    }

    fn size(&self, ty: Ty) -> u32 {
        match ty {
            Ty::Agg(a) => self.aggs.get(a.0 as usize).map_or(u32::MAX / 2, |l| l.size),
            t => t.scalar_size().unwrap_or(0),
        }
    }

    /// The frame bytes `place` accesses (`None`: not rooted at the frame, or only the pointer
    /// value itself is used).
    pub fn target(&self, place: &Place) -> Option<Target> {
        let base: &[u32] = if place.local == self.param {
            &[]
        } else {
            self.derived.get(&place.local)?
        };
        let Some(Proj::Deref(view)) = place.proj.first() else {
            return None;
        };
        let (base_ty, base_off) = self.part(base)?;
        if *view == base_ty {
            if let Some(slot) = self.slot(base, base_ty, base_off, &place.proj) {
                return Some(slot);
            }
        }
        Some(Target::Other(self.other_range(
            *view,
            base_off,
            &place.proj,
        )))
    }

    /// Type of the frame part `place` names field by field (`(*state as State).f.g`, or the
    /// same through a derived pointer), `None` for other places.
    pub fn part_type(&self, place: &Place) -> Option<Ty> {
        let base: &[u32] = if place.local == self.param {
            &[]
        } else {
            self.derived.get(&place.local)?
        };
        let (Some(Proj::Deref(view)), (ty, _)) = (place.proj.first(), self.part(base)?) else {
            return None;
        };
        if *view != ty {
            return None;
        }
        let mut path = base.to_vec();
        for p in &place.proj[1..] {
            let Proj::Field(n) = p else { return None };
            path.push(*n);
        }
        self.part(&path).map(|(t, _)| t)
    }

    /// Field-by-field walk from the part at `base` down to its first scalar.
    fn slot(&self, base: &[u32], mut ty: Ty, mut off: u32, proj: &[Proj]) -> Option<Target> {
        let mut path = base.to_vec();
        let mut end = 1;
        while let Ty::Agg(a) = ty {
            let Some(Proj::Field(n)) = proj.get(end) else {
                return None;
            };
            let (t, o) = *self.aggs.get(a.0 as usize)?.fields.get(*n as usize)?;
            ty = t;
            off += o;
            path.push(*n);
            end += 1;
        }
        let range = (off, off + self.size(ty));
        Some(Target::Slot {
            path,
            ty,
            range,
            end,
        })
    }

    /// Bytes of a non-slot access: the view `ty` at `off`, narrowed by fields, up to the first
    /// dereference (the rest of the place is not frame memory).
    fn other_range(&self, mut ty: Ty, mut off: u32, proj: &[Proj]) -> Range {
        for p in &proj[1..] {
            match (p, ty) {
                (Proj::Field(n), Ty::Agg(a)) => {
                    match self
                        .aggs
                        .get(a.0 as usize)
                        .and_then(|l| l.fields.get(*n as usize))
                    {
                        Some(&(t, o)) => {
                            ty = t;
                            off += o;
                        }
                        None => return (0, u32::MAX),
                    }
                }
                (Proj::Cast(a), _) => ty = Ty::Agg(*a),
                (Proj::Deref(_), _) => break,
                _ => return (0, u32::MAX),
            }
        }
        (off, off.saturating_add(self.size(ty)))
    }
}

/// `q = &(*frame as State).f.g…` definitions of pointer locals assigned exactly once.
pub(super) fn derived_pointers(frame: &Frame, func: &Function) -> HashMap<Local, Vec<u32>> {
    let usage = Usage::of(func);
    let mut out = HashMap::new();
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        let Stmt::Assign(dst, Rvalue::AddrOf(src)) = s else {
            continue;
        };
        let u = usage.get(dst.local);
        if !dst.proj.is_empty()
            || (dst.local.0 as usize) < func.params.len()
            || u.defs != 1
            || u.address_taken
            || src.local != frame.param
            || src.proj.first() != Some(&Proj::Deref(Ty::Agg(frame.state)))
        {
            continue;
        }
        let path: Option<Vec<u32>> = src.proj[1..]
            .iter()
            .map(|p| match p {
                Proj::Field(n) => Some(*n),
                _ => None,
            })
            .collect();
        if let Some(path) = path.filter(|p| frame.part(p).is_some()) {
            out.insert(dst.local, path);
        }
    }
    out
}

/// How the function uses one slot.
#[derive(Debug)]
pub(super) struct SlotUse {
    pub path: Vec<u32>,
    pub ty: Ty,
    pub range: Range,
    pub read: bool,
    pub written: bool,
    /// Accessed in a block that lies on a CFG cycle.
    pub in_loop: bool,
}

/// Every slot the function accesses, and the frame ranges whose address is handed out.
#[derive(Debug, Default)]
pub(super) struct Facts {
    pub slots: Vec<SlotUse>,
    pub exposed: Vec<Range>,
    /// Some part's address is passed to a call (which may keep it; memory copies do not).
    pub passed_on: bool,
}

/// Scan the function; `None` when it uses the frame in a way promotion cannot account for
/// (the frame pointer itself copied, stored or passed on, or a part's address kept anywhere
/// but in a derived pointer, a call argument or a memory-copy operand).
pub(super) fn scan(frame: &Frame, func: &Function, in_loop: &[bool]) -> Option<Facts> {
    let mut sc = Scanner {
        frame,
        facts: Facts::default(),
        in_loop: false,
        ok: true,
    };
    for (bi, block) in func.blocks.iter().enumerate() {
        sc.in_loop = in_loop[bi];
        for s in &block.stmts {
            sc.stmt(s);
        }
        sc.term(&block.term);
    }
    sc.ok.then_some(sc.facts)
}

struct Scanner<'a, 'b> {
    frame: &'a Frame<'b>,
    facts: Facts,
    in_loop: bool,
    ok: bool,
}

impl Scanner<'_, '_> {
    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Assign(dst, rv) => {
                if let Rvalue::AddrOf(src) = rv {
                    let derived_def = self.frame.derived.contains_key(&dst.local);
                    if self.frame.is_root(src.local) && !derived_def {
                        self.ok = false;
                    }
                    if derived_def {
                        return;
                    }
                }
                crate::visit::rvalue_operands(rv, &mut |op| self.operand(op, false));
                self.place(dst, true);
            }
            Stmt::MemCopy { dst, src, .. } => {
                self.operand(dst, true);
                self.operand(src, true);
            }
            Stmt::MemCopyDyn { dst, src, len, .. } => {
                self.operand(dst, true);
                self.operand(src, true);
                self.operand(len, false);
            }
            Stmt::MemSet { dst, byte, len } => {
                self.operand(dst, true);
                self.operand(byte, false);
                self.operand(len, false);
            }
            Stmt::Nop => {}
        }
    }

    fn term(&mut self, t: &Terminator) {
        match t {
            Terminator::Call {
                callee, args, dest, ..
            } => {
                if let Callee::Ptr { target, .. } = callee {
                    self.operand(target, false);
                }
                for a in args {
                    let before = self.facts.exposed.len();
                    self.operand(a, true);
                    self.facts.passed_on |= self.facts.exposed.len() != before;
                }
                if let Some(d) = dest {
                    self.place(d, true);
                }
            }
            t => crate::visit::term_operands(t, &mut |op| self.operand(op, false)),
        }
    }

    /// An operand; `exposing`: a bare derived pointer here hands its part's address to a call
    /// or a memory copy (the part is then never promoted).
    fn operand(&mut self, op: &Operand, exposing: bool) {
        let Operand::Copy(pl) = op else { return };
        if pl.proj.is_empty() && self.frame.is_root(pl.local) {
            match self.frame.derived.get(&pl.local) {
                Some(path) if exposing => {
                    let range = match self.frame.part(path) {
                        Some((ty, off)) => (off, off + self.frame.size(ty)),
                        None => (0, u32::MAX),
                    };
                    self.facts.exposed.push(range);
                }
                _ => self.ok = false,
            }
            return;
        }
        self.place(pl, false);
    }

    fn place(&mut self, pl: &Place, write: bool) {
        if !self.frame.is_root(pl.local) {
            return;
        }
        match self.frame.target(pl) {
            None => self.ok = false,
            Some(Target::Other(_)) => {}
            Some(Target::Slot {
                path,
                ty,
                range,
                end,
            }) => {
                let writes = write && end == pl.proj.len();
                let i = match self.facts.slots.iter().position(|s| s.path == path) {
                    Some(i) => i,
                    None => {
                        self.facts.slots.push(SlotUse {
                            path,
                            ty,
                            range,
                            read: false,
                            written: false,
                            in_loop: false,
                        });
                        self.facts.slots.len() - 1
                    }
                };
                let s = &mut self.facts.slots[i];
                s.written |= writes;
                s.read |= !writes;
                s.in_loop |= self.in_loop;
            }
        }
    }
}
