//! Promotion of `noalias` pointees into locals (redundant-load elimination for params the
//! function modifies).
//!
//! A `noalias` param (vir.rs invariant 9: e.g. `xs: T[]` of a function that pushes to it) points to memory that nothing
//! but pointers derived from the param touches while the function runs. When the function only
//! uses the param to access scalar fields of its pointee (`(*p).0`, `(*p).1.2`), and otherwise
//! only hands `p` itself to calls, every field it reads behaves like a local variable:
//! - it is loaded once on entry into a new local, and reads and writes use the local;
//! - before a call that receives `p` the promoted fields written so far are stored back, and
//!   after it all promoted fields are reloaded (the callee may change them);
//! - before every `return` the written fields are stored back.
//!
//! Stores through other pointers (array elements through a data pointer loaded from `*p`)
//! cannot change `*p`, so an array's data pointer and length stay in registers across its
//! element stores. LLVM cannot prove that by itself: the data pointer was loaded from memory,
//! and `p` may have been captured by an earlier call. Anything that exposes the pointee
//! otherwise (`&(*p).f`, whole-aggregate accesses, enum views, `p` stored or used anywhere but
//! as a call argument) leaves the param alone.

use velt_vir::vir::{
    AggLayout, BasicBlock, BlockId, Callee, Function, Local, LocalDecl, Operand, Place, Proj,
    Rvalue, Stmt, Terminator, Ty,
};

use crate::srclocs::{prepend_stmts, push_stmt, replace_block};
use crate::visit::{places_mut, stmt_operands, successors, successors_mut, term_operands};

/// Promote the pointees of the `noalias` params of `func`; returns whether anything changed.
pub(crate) fn run(aggs: &[AggLayout], func: &mut Function) -> bool {
    let mut changed = false;
    for i in 0..func.params.len() {
        if func.params[i] != Ty::Ptr || !func.param_attr(i).noalias {
            continue;
        }
        let mut scan = Scan {
            aggs,
            p: Local(i as u32),
            pointee: None,
            fields: vec![],
            escapes: false,
        };
        scan.function(func);
        let Some(pointee) = scan.pointee.filter(|_| !scan.escapes) else {
            continue;
        };
        // Fields that are only written need no local: their stores stay stores.
        let fields: Vec<Field> = scan.fields.into_iter().filter(|f| f.read).collect();
        if !fields.is_empty() {
            if !changed {
                fresh_entry(func);
            }
            promote(func, Local(i as u32), pointee, fields);
            changed = true;
        }
    }
    changed
}

/// A scalar field of the pointee reached by `path`, and how the function uses it.
struct Field {
    path: Vec<u32>,
    ty: Ty,
    read: bool,
    written: bool,
    /// The promoted local (set by `promote`).
    local: Local,
}

/// How a function uses param `p`.
struct Scan<'a> {
    aggs: &'a [AggLayout],
    p: Local,
    /// The pointee type all dereferences of `p` agree on.
    pointee: Option<Ty>,
    fields: Vec<Field>,
    /// `p` is used in a way promotion cannot express.
    escapes: bool,
}

impl Scan<'_> {
    fn function(&mut self, func: &Function) {
        for block in &func.blocks {
            for s in &block.stmts {
                self.stmt(s);
            }
            self.term(&block.term);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        stmt_operands(s, &mut |op| self.operand(op));
        if let Stmt::Assign(dst, rv) = s {
            if let Rvalue::AddrOf(a) = rv {
                // `&(*p).f` would be a pointer derived from `p`.
                self.escapes |= a.local == self.p;
            }
            self.place(dst, true);
        }
    }

    fn term(&mut self, t: &Terminator) {
        match t {
            Terminator::Call {
                callee, args, dest, ..
            } => {
                if let Callee::Ptr { target, .. } = callee {
                    self.operand(target);
                }
                for a in args {
                    if !self.is_bare(a) {
                        self.operand(a);
                    }
                }
                if let Some(d) = dest {
                    self.place(d, true);
                }
            }
            t => term_operands(t, &mut |op| self.operand(op)),
        }
    }

    fn is_bare(&self, op: &Operand) -> bool {
        matches!(op, Operand::Copy(pl) if pl.local == self.p && pl.proj.is_empty())
    }

    fn operand(&mut self, op: &Operand) {
        if let Operand::Copy(pl) = op {
            self.place(pl, false);
        }
    }

    /// A place read (`write` false) or assigned (`write` true).
    fn place(&mut self, pl: &Place, write: bool) {
        if pl.local != self.p {
            return;
        }
        let Some((pointee, path, ty, end)) = leaf_path(self.aggs, pl) else {
            self.escapes = true;
            return;
        };
        if self.pointee.is_some_and(|t| t != pointee) {
            self.escapes = true;
            return;
        }
        self.pointee = Some(pointee);
        // Assigning through a pointer stored in the field (`(*(*p).0 as T) = v`) reads it.
        let writes_field = write && end == pl.proj.len();
        let i = match self.fields.iter().position(|f| f.path == path) {
            Some(i) => i,
            None => {
                self.fields.push(Field {
                    path,
                    ty,
                    read: false,
                    written: false,
                    local: Local(0),
                });
                self.fields.len() - 1
            }
        };
        let f = &mut self.fields[i];
        f.written |= writes_field;
        f.read |= !writes_field;
    }
}

/// `(*p as T).f.g…` down to its first scalar: (T, field path, scalar type, number of
/// projections up to it); `None` for `p` itself, aggregate accesses and enum views.
fn leaf_path(aggs: &[AggLayout], place: &Place) -> Option<(Ty, Vec<u32>, Ty, usize)> {
    let Some(Proj::Deref(t0)) = place.proj.first() else {
        return None;
    };
    let mut ty = *t0;
    let mut path = vec![];
    while let Ty::Agg(a) = ty {
        let Some(Proj::Field(n)) = place.proj.get(path.len() + 1) else {
            return None;
        };
        ty = aggs.get(a.0 as usize)?.fields.get(*n as usize)?.0;
        path.push(*n);
    }
    let end = path.len() + 1;
    Some((*t0, path, ty, end))
}

/// `(*p as T).path`
fn field_place(p: Local, pointee: Ty, path: &[u32]) -> Place {
    let mut proj = vec![Proj::Deref(pointee)];
    proj.extend(path.iter().map(|n| Proj::Field(*n)));
    Place { local: p, proj }
}

fn promote(func: &mut Function, p: Local, pointee: Ty, mut fields: Vec<Field>) {
    for f in &mut fields {
        func.locals.push(LocalDecl::new(f.ty, None));
        f.local = Local(func.locals.len() as u32 - 1);
    }
    // Rewrite the accesses first: the loads and stores added below must keep naming `*p`.
    places_mut(func, &mut |pl| {
        if pl.local != p {
            return;
        }
        let hit = fields.iter().find(|f| {
            pl.proj.len() > f.path.len()
                && pl.proj[1..=f.path.len()]
                    .iter()
                    .zip(&f.path)
                    .all(|(x, n)| *x == Proj::Field(*n))
        });
        if let Some(f) = hit {
            let rest = pl.proj.split_off(f.path.len() + 1);
            *pl = Place {
                local: f.local,
                proj: rest,
            };
        }
    });
    let load = |f: &Field| {
        let from = field_place(p, pointee, &f.path);
        Stmt::Assign(Place::local(f.local), Rvalue::Use(Operand::Copy(from)))
    };
    let store = |f: &Field| {
        let from = Operand::Copy(Place::local(f.local));
        Stmt::Assign(field_place(p, pointee, &f.path), Rvalue::Use(from))
    };
    prepend_stmts(func, 0, fields.iter().map(load).collect());
    for bi in 0..func.blocks.len() {
        let exposes = match &func.blocks[bi].term {
            Terminator::Call { args, .. } => args
                .iter()
                .any(|a| matches!(a, Operand::Copy(pl) if pl.local == p && pl.proj.is_empty())),
            Terminator::Return(_) => true,
            _ => false,
        };
        if !exposes {
            continue;
        }
        let at = func.term_loc(bi);
        for f in fields.iter().filter(|f| f.written) {
            push_stmt(func, bi, store(f), at);
        }
        if let Terminator::Call { dest, next, .. } = &func.blocks[bi].term {
            // A field the call's result lands in keeps that result.
            let dest = dest.as_ref().filter(|d| d.proj.is_empty()).map(|d| d.local);
            let reloads = fields
                .iter()
                .filter(|f| Some(f.local) != dest)
                .map(load)
                .collect();
            let target = *next;
            let reload_bb = add_block(func, reloads, Terminator::Goto(target));
            if let Terminator::Call { next, .. } = &mut func.blocks[bi].term {
                *next = reload_bb;
            }
        }
    }
}

/// Make sure nothing jumps to the entry block (a loop header after CFG simplification), so the
/// entry loads run once: its contents move to a new block the entry jumps to.
pub(crate) fn fresh_entry(func: &mut Function) {
    let entry = BlockId(0);
    if !func
        .blocks
        .iter()
        .any(|b| successors(&b.term).contains(&entry))
    {
        return;
    }
    let (old, locs) = replace_block(
        func,
        0,
        BasicBlock {
            stmts: vec![],
            term: Terminator::Unreachable,
        },
    );
    let moved = BlockId(func.blocks.len() as u32);
    for b in &mut func.blocks {
        successors_mut(&mut b.term, &mut |t| {
            if *t == entry {
                *t = moved;
            }
        });
    }
    func.blocks.push(old);
    if !func.locs.is_empty() {
        func.locs.push(locs);
    }
    func.blocks[0].term = Terminator::Goto(moved);
}

/// Append a block (without source locations).
pub(crate) fn add_block(func: &mut Function, stmts: Vec<Stmt>, term: Terminator) -> BlockId {
    if !func.locs.is_empty() {
        func.locs.push(vec![None; stmts.len() + 1]);
    }
    func.blocks.push(BasicBlock { stmts, term });
    BlockId(func.blocks.len() as u32 - 1)
}

#[cfg(test)]
mod tests;
