//! Finding the webs of pointer locals whose objects never escape (module docs of `heap_sroa`):
//! pointer locals are grouped by the copies between them, and a group is kept only when every
//! mention of its locals is one the rewrite understands.

use velt_vir::vir::{
    AggId, AggLayout, BinOp, Callee, Const, Function, Local, Operand, Place, Proj, Rvalue, Stmt,
    Terminator, Ty,
};

use super::Allocator;
use crate::sroa::{fields_cover, MAX_FIELDS};
use crate::visit::{rvalue_operands, stmt_operands, term_operands};

/// The webs of one function that may be replaced.
pub(super) struct Webs {
    /// Per local: the web it belongs to (an index into `objs`).
    web_of: Vec<Option<u32>>,
    /// Per web: the object aggregate its pointers point to, or `None` once disqualified.
    objs: Vec<Option<AggId>>,
}

impl Webs {
    /// The object aggregate behind `l`, if `l` belongs to a web still being replaced.
    pub fn obj(&self, l: Local) -> Option<AggId> {
        let web = (*self.web_of.get(l.0 as usize)?)?;
        self.objs[web as usize]
    }

    /// The web of `l` (replaced or not).
    pub fn web(&self, l: Local) -> Option<u32> {
        self.web_of.get(l.0 as usize).copied().flatten()
    }

    /// Keep the objects of `l`'s web on the heap.
    pub fn disqualify(&mut self, l: Local) {
        if let Some(web) = self.web(l) {
            self.objs[web as usize] = None;
        }
    }

    /// Whether any web is still replaced.
    pub fn any(&self) -> bool {
        self.objs.iter().any(Option::is_some)
    }
}

/// A constant size (and alignment) a web's allocations, frees and fills use.
enum Size {
    /// `velt_rt_alloc` / `velt_rt_free` arguments.
    Block(i128, i128),
    /// `memset` length.
    Fill(i128),
}

/// What one group of copy-connected pointer locals is used for.
#[derive(Default)]
struct Facts {
    bad: bool,
    allocated: bool,
    obj: Option<AggId>,
    sizes: Vec<Size>,
}

/// Group the pointer locals of `func` and keep the groups whose objects can live in locals;
/// `None` when there is none.
pub(super) fn scan(aggs: &[AggLayout], allocator: Allocator, func: &Function) -> Option<Webs> {
    let allocates = func.blocks.iter().any(|b| {
        matches!(&b.term, Terminator::Call { callee: Callee::Extern(e), .. } if *e == allocator.alloc)
    });
    if !allocates {
        return None;
    }
    let n = func.locals.len();
    let cand: Vec<bool> = (0..n)
        .map(|i| i >= func.params.len() && func.locals[i].ty == Ty::Ptr)
        .collect();
    let mut groups = Groups::new(n);
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        if let Stmt::Assign(dst, Rvalue::Use(Operand::Copy(src))) = s {
            let whole = dst.proj.is_empty() && src.proj.is_empty();
            if whole && cand[dst.local.0 as usize] && cand[src.local.0 as usize] {
                groups.union(dst.local.0 as usize, src.local.0 as usize);
            }
        }
    }
    let mut scanner = Scanner {
        allocator,
        cand,
        facts: (0..n).map(|_| Facts::default()).collect(),
        groups,
    };
    for block in &func.blocks {
        for s in &block.stmts {
            scanner.stmt(s);
        }
        scanner.term(&block.term);
    }
    scanner.webs(aggs)
}

/// Union-find over locals.
struct Groups {
    parent: Vec<usize>,
}

impl Groups {
    fn new(n: usize) -> Groups {
        Groups {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        self.parent[a] = b;
    }
}

struct Scanner {
    allocator: Allocator,
    cand: Vec<bool>,
    facts: Vec<Facts>,
    groups: Groups,
}

fn is_null(op: &Operand) -> bool {
    matches!(op, Operand::Const(Const::Int(0), Ty::Ptr))
}

fn constant(op: &Operand) -> Option<i128> {
    match op {
        Operand::Const(Const::Int(v), _) => Some(*v),
        _ => None,
    }
}

impl Scanner {
    fn is_cand(&self, l: Local) -> bool {
        self.cand.get(l.0 as usize).copied().unwrap_or(false)
    }

    /// The local itself (no projection), if it is a candidate.
    fn whole(&self, p: &Place) -> Option<Local> {
        (p.proj.is_empty() && self.is_cand(p.local)).then_some(p.local)
    }

    fn facts(&mut self, l: Local) -> &mut Facts {
        let root = self.groups.find(l.0 as usize);
        &mut self.facts[root]
    }

    fn bad(&mut self, l: Local) {
        self.facts(l).bad = true;
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Assign(dst, rv) => {
                match self.whole(dst) {
                    Some(_) if self.whole_def(rv) => return,
                    Some(d) => self.bad(d),
                    None => self.place(dst),
                }
                self.rvalue(rv);
            }
            Stmt::MemSet { dst, byte, len } => {
                let fill = match dst {
                    Operand::Copy(p) => self.whole(p).filter(|_| constant(byte) == Some(0)),
                    Operand::Const(..) => None,
                };
                match (fill, constant(len)) {
                    (Some(w), Some(n)) => self.facts(w).sizes.push(Size::Fill(n)),
                    _ => stmt_operands(s, &mut |op| self.operand(op)),
                }
            }
            _ => stmt_operands(s, &mut |op| self.operand(op)),
        }
    }

    /// A whole assignment to a candidate keeps it in its web: a copy of another candidate
    /// (grouped already) or null.
    fn whole_def(&self, rv: &Rvalue) -> bool {
        match rv {
            Rvalue::Use(Operand::Copy(src)) => self.whole(src).is_some(),
            Rvalue::Use(op) => is_null(op),
            _ => false,
        }
    }

    fn rvalue(&mut self, rv: &Rvalue) {
        match rv {
            Rvalue::Binary(BinOp::Eq | BinOp::Ne, a, b) if is_null(a) || is_null(b) => {
                for op in [a, b] {
                    match op {
                        Operand::Copy(p) if self.whole(p).is_some() => {}
                        _ => self.operand(op),
                    }
                }
            }
            Rvalue::AddrOf(p) if self.is_cand(p.local) => self.bad(p.local),
            _ => rvalue_operands(rv, &mut |op| self.operand(op)),
        }
    }

    /// A read anywhere else: the pointer itself escapes, a place through it is an access.
    fn operand(&mut self, op: &Operand) {
        let Operand::Copy(p) = op else { return };
        match self.whole(p) {
            Some(w) => self.bad(w),
            None => self.place(p),
        }
    }

    /// An access through a candidate must name its object aggregate first.
    fn place(&mut self, p: &Place) {
        if !self.is_cand(p.local) {
            return;
        }
        let facts = self.facts(p.local);
        match (p.proj.first(), facts.obj) {
            (Some(Proj::Deref(Ty::Agg(id))), None) => facts.obj = Some(*id),
            (Some(Proj::Deref(Ty::Agg(id))), Some(obj)) if *id == obj => {}
            _ => facts.bad = true,
        }
    }

    fn term(&mut self, t: &Terminator) {
        if let Terminator::Call {
            callee: Callee::Extern(e),
            args,
            dest,
            ..
        } = t
        {
            if *e == self.allocator.alloc {
                if let (Some(w), [size, align]) =
                    (dest.as_ref().and_then(|d| self.whole(d)), &args[..])
                {
                    if let (Some(size), Some(align)) = (constant(size), constant(align)) {
                        let facts = self.facts(w);
                        facts.allocated = true;
                        facts.sizes.push(Size::Block(size, align));
                        return;
                    }
                }
            }
            if *e == self.allocator.free {
                if let [Operand::Copy(p), size, align] = &args[..] {
                    let sizes = (constant(size), constant(align));
                    if let (Some(w), (Some(size), Some(align))) = (self.whole(p), sizes) {
                        self.facts(w).sizes.push(Size::Block(size, align));
                        return;
                    }
                }
            }
        }
        term_operands(t, &mut |op| self.operand(op));
        if let Terminator::Call { dest: Some(d), .. } = t {
            match self.whole(d) {
                Some(w) => self.bad(w),
                None => self.place(d),
            }
        }
    }

    /// Number the groups that pass, with their object aggregates.
    fn webs(mut self, aggs: &[AggLayout]) -> Option<Webs> {
        let n = self.cand.len();
        let mut web_of = vec![None; n];
        let mut objs = Vec::new();
        let mut root_web: Vec<Option<u32>> = vec![None; n];
        for (l, slot) in web_of.iter_mut().enumerate() {
            if !self.cand[l] {
                continue;
            }
            let root = self.groups.find(l);
            let Some(obj) = replaceable(aggs, &self.facts[root]) else {
                continue;
            };
            let web = *root_web[root].get_or_insert_with(|| {
                objs.push(Some(obj));
                objs.len() as u32 - 1
            });
            *slot = Some(web);
        }
        (!objs.is_empty()).then_some(Webs { web_of, objs })
    }
}

/// The object aggregate of a group whose uses all fit, with sizes matching its layout and a
/// layout `sroa` can split.
fn replaceable(aggs: &[AggLayout], facts: &Facts) -> Option<AggId> {
    if facts.bad || !facts.allocated {
        return None;
    }
    let obj = facts.obj?;
    let layout = aggs.get(obj.0 as usize)?;
    let (size, align) = (i128::from(layout.size), i128::from(layout.align));
    let sizes_fit = facts.sizes.iter().all(|s| match *s {
        Size::Block(s, a) => s == size && a == align,
        Size::Fill(s) => s == size,
    });
    let splittable = (1..=MAX_FIELDS).contains(&layout.fields.len()) && fields_cover(aggs, layout);
    (sizes_fit && splittable).then_some(obj)
}
