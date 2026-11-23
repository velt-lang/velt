//! Finding the webs of pointer locals whose objects never escape (module docs of `heap_sroa`):
//! pointer locals are grouped by the copies between them, and a group is kept only when every
//! mention of its locals is one the rewrite understands.

use velt_vir::vir::{
    AggId, AggLayout, BinOp, Callee, Const, Function, Local, Operand, Place, Proj, Rvalue,
    StaticId, Stmt, Terminator, Ty,
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
    /// Per web: whether its blocks hold a reference count in the 8 bytes before the object.
    counted: Vec<bool>,
    /// Per local: the vtable of the objects it holds, when its group would be replaced but for
    /// calls through that vtable that are given the object (`devirt`).
    pub vtable_of: Vec<Option<StaticId>>,
}

impl Webs {
    /// The object aggregate behind `l`, if `l` belongs to a web still being replaced.
    pub fn obj(&self, l: Local) -> Option<AggId> {
        let web = (*self.web_of.get(l.0 as usize)?)?;
        self.objs[web as usize]
    }

    /// Whether `l`'s web holds counted objects (a `u64` count, then the object).
    pub fn counted(&self, l: Local) -> bool {
        self.web(l).is_some_and(|w| self.counted[w as usize])
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

/// A constant size (and alignment) a web's allocations, frees and fills use, with the offset
/// (relative to the group's root, see [`Groups`]) of the pointer it is given.
enum Size {
    /// `velt_rt_alloc` result / `velt_rt_free` argument and its size and alignment.
    Block(i128, i128, i128),
    /// `memset` start and length.
    Fill(i128, i128),
}

/// What one group of copy-connected pointer locals is used for. Offsets are relative to the
/// group's root.
#[derive(Default)]
struct Facts {
    bad: bool,
    allocated: bool,
    obj: Option<AggId>,
    /// Where the object is accessed (`(*w as Obj)`), which must be one offset.
    obj_at: Option<i128>,
    /// Other aggregates the object is accessed as, at the same offset: kept only when their
    /// layout is the object's.
    alike: Vec<AggId>,
    /// Where `(*w as u64)` is accessed: the count in front of a counted object.
    counts: Vec<i128>,
    sizes: Vec<Size>,
    /// Indirect calls given the object alone (`call f(w)`): a drop or method through its vtable.
    calls: bool,
    /// What is stored in the object's first field (`Some` for a static's address).
    firsts: Vec<Option<StaticId>>,
    /// The whole object is stored at once (its first field with it).
    stored_whole: bool,
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
    let mut clash = Vec::new();
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        let Stmt::Assign(dst, rv) = s else { continue };
        if let Some((src, by)) = moved(rv) {
            let whole = dst.proj.is_empty() && src.proj.is_empty();
            if whole && cand[dst.local.0 as usize] && cand[src.local.0 as usize] {
                let (d, s) = (dst.local.0 as usize, src.local.0 as usize);
                if !groups.union(d, s, by) {
                    clash.push(d);
                }
            }
        }
    }
    let mut scanner = Scanner {
        allocator,
        cand,
        facts: (0..n).map(|_| Facts::default()).collect(),
        groups,
    };
    for d in clash {
        scanner.bad(Local(d as u32));
    }
    for block in &func.blocks {
        for s in &block.stmts {
            scanner.stmt(s);
        }
        scanner.term(&block.term);
    }
    scanner.webs(aggs)
}

/// `src` and the constant it is moved by, for `d = src` (0) and `d = src + c`.
pub(super) fn moved(rv: &Rvalue) -> Option<(&Place, i128)> {
    match rv {
        Rvalue::Use(Operand::Copy(src)) => Some((src, 0)),
        Rvalue::Binary(BinOp::PtrAdd, Operand::Copy(src), by) => Some((src, constant(by)?)),
        _ => None,
    }
}

/// Union-find over locals, with each local's offset from its root in bytes (`d = s + c` puts
/// `d` at `s`'s offset plus `c`).
struct Groups {
    parent: Vec<usize>,
    /// Offset from the parent.
    by: Vec<i128>,
}

impl Groups {
    fn new(n: usize) -> Groups {
        Groups {
            parent: (0..n).collect(),
            by: vec![0; n],
        }
    }

    /// The root of `x` and `x`'s offset from it.
    fn find(&mut self, x: usize) -> (usize, i128) {
        // Two passes, no allocation: find the root and `x`'s offset from it, then point each
        // node on the path at the root with its own offset (what is left of `x`'s after the
        // steps below it).
        let (mut root, mut total) = (x, 0);
        while self.parent[root] != root {
            total += self.by[root];
            root = self.parent[root];
        }
        let (mut node, mut off) = (x, total);
        while node != root {
            let (next, step) = (self.parent[node], self.by[node]);
            self.parent[node] = root;
            self.by[node] = off;
            off -= step;
            node = next;
        }
        (root, total)
    }

    /// Record `a = b + by`; false when the two were grouped already at other offsets.
    fn union(&mut self, a: usize, b: usize, by: i128) -> bool {
        let ((ra, oa), (rb, ob)) = (self.find(a), self.find(b));
        if ra == rb {
            return oa == ob + by;
        }
        self.parent[ra] = rb;
        self.by[ra] = ob + by - oa;
        true
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
        self.facts_at(l).0
    }

    /// `l`'s group facts and `l`'s offset from the group's root.
    fn facts_at(&mut self, l: Local) -> (&mut Facts, i128) {
        let (root, at) = self.groups.find(l.0 as usize);
        (&mut self.facts[root], at)
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
                    None => {
                        self.place(dst);
                        self.first_store(dst, rv);
                    }
                }
                self.rvalue(rv);
            }
            Stmt::MemSet { dst, byte, len } => {
                let fill = match dst {
                    Operand::Copy(p) => self.whole(p).filter(|_| constant(byte) == Some(0)),
                    Operand::Const(..) => None,
                };
                match (fill, constant(len)) {
                    (Some(w), Some(n)) => {
                        let (facts, at) = self.facts_at(w);
                        facts.sizes.push(Size::Fill(at, n));
                    }
                    _ => stmt_operands(s, &mut |op| self.operand(op)),
                }
            }
            _ => stmt_operands(s, &mut |op| self.operand(op)),
        }
    }

    /// A whole assignment to a candidate keeps it in its web: a copy of another candidate, or
    /// one moved by a constant (grouped already), or null.
    fn whole_def(&self, rv: &Rvalue) -> bool {
        match (rv, moved(rv)) {
            (_, Some((src, _))) => self.whole(src).is_some(),
            (Rvalue::Use(op), None) => is_null(op),
            _ => false,
        }
    }

    /// A store into the object's first field (its vtable, in a class with one), or of the
    /// whole object.
    fn first_store(&mut self, dst: &Place, rv: &Rvalue) {
        if !self.is_cand(dst.local) {
            return;
        }
        match dst.proj.as_slice() {
            [Proj::Deref(Ty::Agg(_)), Proj::Field(0)] => {
                let first = match rv {
                    Rvalue::Use(Operand::Const(Const::Static(id), Ty::Ptr)) => Some(*id),
                    _ => None,
                };
                self.facts(dst.local).firsts.push(first);
            }
            [Proj::Deref(Ty::Agg(_))] => self.facts(dst.local).stored_whole = true,
            _ => {}
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

    /// An access through a candidate must name its object aggregate first, or be the whole
    /// `u64` count in front of it.
    fn place(&mut self, p: &Place) {
        if !self.is_cand(p.local) {
            return;
        }
        let (facts, at) = self.facts_at(p.local);
        match p.proj.as_slice() {
            [Proj::Deref(Ty::U64)] => facts.counts.push(at),
            [Proj::Deref(Ty::Agg(id)), ..] => match (facts.obj, facts.obj_at) {
                (None, _) => (facts.obj, facts.obj_at) = (Some(*id), Some(at)),
                (Some(obj), Some(o)) if o == at => {
                    if obj != *id && !facts.alike.contains(id) {
                        facts.alike.push(*id);
                    }
                }
                _ => facts.bad = true,
            },
            _ => facts.bad = true,
        }
    }

    fn term(&mut self, t: &Terminator) {
        if let Terminator::Call {
            callee: Callee::Ptr { target, .. },
            args,
            dest,
            ..
        } = t
        {
            if let [Operand::Copy(p)] = &args[..] {
                if let Some(w) = self.whole(p) {
                    // A drop, or a getter (`buffer`, `byteLength`) through the vtable.
                    self.facts(w).calls = true;
                    self.operand(target);
                    if let Some(d) = dest {
                        match self.whole(d) {
                            Some(r) => self.bad(r),
                            None => self.place(d),
                        }
                    }
                    return;
                }
            }
        }
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
                        let (facts, at) = self.facts_at(w);
                        facts.allocated = true;
                        facts.sizes.push(Size::Block(at, size, align));
                        return;
                    }
                }
            }
            if *e == self.allocator.free {
                if let [Operand::Copy(p), size, align] = &args[..] {
                    let sizes = (constant(size), constant(align));
                    if let (Some(w), (Some(size), Some(align))) = (self.whole(p), sizes) {
                        let (facts, at) = self.facts_at(w);
                        facts.sizes.push(Size::Block(at, size, align));
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
        let mut counted = Vec::new();
        let mut root_web: Vec<Option<u32>> = vec![None; n];
        let mut vtable_of = vec![None; n];
        for (l, slot) in web_of.iter_mut().enumerate() {
            if !self.cand[l] {
                continue;
            }
            let (root, _) = self.groups.find(l);
            let facts = &self.facts[root];
            let Some(fit) = replaceable(aggs, facts) else {
                continue;
            };
            if facts.calls {
                vtable_of[l] = vtable(aggs, fit.obj, facts);
                continue;
            }
            let web = *root_web[root].get_or_insert_with(|| {
                objs.push(Some(fit.obj));
                counted.push(fit.counted);
                objs.len() as u32 - 1
            });
            *slot = Some(web);
        }
        let devirt = vtable_of.iter().any(Option::is_some);
        (!objs.is_empty() || devirt).then_some(Webs {
            web_of,
            objs,
            counted,
            vtable_of,
        })
    }
}

/// How a group's blocks hold its objects.
struct Fit {
    obj: AggId,
    /// A `u64` count at the start of each block, the object 8 bytes after it.
    counted: bool,
}

/// How a group whose uses all fit holds its objects: one object aggregate (or ones laid out as
/// it), `sroa` can split, at the start of every block it allocates, or 8 bytes in, after a
/// `u64` count accessed only as a whole; with every size and offset matching.
fn replaceable(aggs: &[AggLayout], facts: &Facts) -> Option<Fit> {
    if facts.bad || !facts.allocated {
        return None;
    }
    let (obj, obj_at) = (facts.obj?, facts.obj_at?);
    let layout = aggs.get(obj.0 as usize)?;
    let laid_out_alike = |a: &AggId| {
        aggs.get(a.0 as usize).is_some_and(|other| {
            (other.size, other.align, &other.fields) == (layout.size, layout.align, &layout.fields)
        })
    };
    if !facts.alike.iter().all(laid_out_alike) {
        return None;
    }
    let start = facts.sizes.iter().find_map(|s| match *s {
        Size::Block(at, ..) => Some(at),
        Size::Fill(..) => None,
    })?;
    let counted = obj_at - start == 8;
    if !(counted || obj_at == start) || (!counted && !facts.counts.is_empty()) {
        return None;
    }
    let block = i128::from(layout.size) + if counted { 8 } else { 0 };
    let (align, obj_size) = (i128::from(layout.align), i128::from(layout.size));
    let sizes_fit = facts.sizes.iter().all(|s| match *s {
        Size::Block(at, s, a) => at == start && s == block && a == align,
        Size::Fill(at, s) => at == obj_at && s == obj_size,
    });
    let counts_fit = facts.counts.iter().all(|&at| at == start);
    let splittable = (1..=MAX_FIELDS).contains(&layout.fields.len()) && fields_cover(aggs, layout);
    (sizes_fit && counts_fit && splittable).then_some(Fit { obj, counted })
}

/// The one vtable a group's objects get (a static's address stored in their first field, a
/// pointer at offset 0, and nothing else ever stored there), if there is one.
fn vtable(aggs: &[AggLayout], obj: AggId, facts: &Facts) -> Option<StaticId> {
    let first = aggs.get(obj.0 as usize)?.fields.first();
    if first != Some(&(Ty::Ptr, 0)) || facts.stored_whole {
        return None;
    }
    let (&head, rest) = facts.firsts.split_first()?;
    let id = head?;
    rest.iter().all(|f| *f == Some(id)).then_some(id)
}
