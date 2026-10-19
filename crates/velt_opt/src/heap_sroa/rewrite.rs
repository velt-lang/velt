//! The rewrite of `heap_sroa` (module docs there): each pointer local of a replaced web gets an
//! object local `w.obj` holding what the heap object held; the pointer stays as a non-null
//! token for the drop guards (`constfold` and `dce` remove it once nothing tests it).

use std::collections::HashMap;

use velt_vir::vir::{
    AggId, AggLayout, Callee, Const, Function, Local, LocalDecl, Operand, Place, Proj, Rvalue,
    Stmt, Terminator, Ty,
};

use super::flow::Update;
use super::webs::Webs;
use super::Allocator;
use crate::srclocs::{prepend_stmts, push_stmt, rewrite_stmts};
use crate::sroa::zero;
use crate::visit::places_mut;

/// Replace the objects of the remaining webs of `func` by locals.
pub(super) fn apply(
    aggs: &[AggLayout],
    allocator: Allocator,
    func: &mut Function,
    webs: &Webs,
    updates: &[Update],
) {
    let mut zeros = Zeros::default();
    let mut objs: Vec<Option<(Local, AggId)>> = vec![None; func.locals.len()];
    let mut entry = Vec::new();
    for (i, slot) in objs.iter_mut().enumerate() {
        let Some(obj) = webs.obj(Local(i as u32)) else {
            continue;
        };
        let name = func.locals[i].name.as_ref().map(|n| format!("{n}.obj"));
        let l = Local(func.locals.len() as u32);
        // The object's fields, not the variable (a pointer): no debug description (only release
        // builds run this, and they describe none).
        func.locals.push(LocalDecl {
            debug: None,
            ..LocalDecl::new(Ty::Agg(obj), name)
        });
        *slot = Some((l, obj));
        entry.push((l, obj));
    }
    let entry: Vec<Stmt> = entry
        .into_iter()
        .map(|(l, obj)| zeros.assign(aggs, func, l, obj))
        .collect();
    let obj_of = |l: Local| objs.get(l.0 as usize).copied().flatten();
    let obj_local = |l: Local| obj_of(l).expect("ICE: heap_sroa update outside a web").0;
    let mut pending = updates.iter().peekable();
    for bi in 0..func.blocks.len() {
        let mut at = 0;
        rewrite_stmts(func, bi, |s, out| {
            match web_stmt(&s, &obj_of) {
                WebStmt::Copy(dst, src) => {
                    out.push(s);
                    out.push(copy(dst, src));
                }
                WebStmt::Fill(l, obj) => out.push(zeros.fill(l, obj)),
                WebStmt::Other => out.push(s),
            }
            while let Some(u) = pending.next_if(|u| (u.block, u.stmt) == (bi, at)) {
                out.push(copy(obj_local(u.dst), obj_local(u.src)));
            }
            at += 1;
        });
        rewrite_term(allocator, func, bi, &obj_of, &zeros);
    }
    places_mut(func, &mut |p| {
        if let (Some((l, _)), Some(Proj::Deref(_))) = (obj_of(p.local), p.proj.first()) {
            *p = Place {
                local: l,
                proj: p.proj[1..].to_vec(),
            };
        }
    });
    let mut inits = std::mem::take(&mut zeros.inits);
    inits.extend(entry);
    prepend_stmts(func, 0, inits);
}

/// How a statement involves the webs.
enum WebStmt {
    /// `d = w`: copy the object along (`d.obj = w.obj`).
    Copy(Local, Local),
    /// `memset w, 0, size`: the object becomes zero.
    Fill(Local, AggId),
    /// Places through `w` (rewritten afterwards), null tests, or nothing.
    Other,
}

fn web_stmt(s: &Stmt, obj_of: &impl Fn(Local) -> Option<(Local, AggId)>) -> WebStmt {
    match s {
        Stmt::Assign(dst, Rvalue::Use(Operand::Copy(src)))
            if dst.proj.is_empty() && src.proj.is_empty() =>
        {
            match (obj_of(dst.local), obj_of(src.local)) {
                (Some((d, _)), Some((w, _))) if d != w => WebStmt::Copy(d, w),
                _ => WebStmt::Other,
            }
        }
        Stmt::MemSet {
            dst: Operand::Copy(p),
            ..
        } if p.proj.is_empty() => match obj_of(p.local) {
            Some((l, obj)) => WebStmt::Fill(l, obj),
            None => WebStmt::Other,
        },
        _ => WebStmt::Other,
    }
}

fn copy(dst: Local, src: Local) -> Stmt {
    Stmt::Assign(
        Place::local(dst),
        Rvalue::Use(Operand::Copy(Place::local(src))),
    )
}

/// An allocation becomes a non-null token and a zero object; a free disappears.
fn rewrite_term(
    allocator: Allocator,
    func: &mut Function,
    bi: usize,
    obj_of: &impl Fn(Local) -> Option<(Local, AggId)>,
    zeros: &Zeros,
) {
    let whole = |p: &Place| p.proj.is_empty().then_some(p.local);
    let (next, alloc) = match &func.blocks[bi].term {
        Terminator::Call {
            callee: Callee::Extern(e),
            dest: Some(d),
            next,
            ..
        } if *e == allocator.alloc => match whole(d).and_then(|w| Some((w, obj_of(w)?))) {
            Some(alloc) => (*next, Some(alloc)),
            None => return,
        },
        Terminator::Call {
            callee: Callee::Extern(e),
            args,
            next,
            ..
        } if *e == allocator.free => match args.first() {
            Some(Operand::Copy(p)) if whole(p).and_then(obj_of).is_some() => (*next, None),
            _ => return,
        },
        _ => return,
    };
    if let Some((w, (l, obj))) = alloc {
        let at = func.term_loc(bi);
        let token = Rvalue::Use(Operand::Const(Const::Int(1), Ty::Ptr));
        push_stmt(func, bi, Stmt::Assign(Place::local(w), token), at);
        push_stmt(func, bi, zeros.fill(l, obj), at);
    }
    func.blocks[bi].term = Terminator::Goto(next);
}

/// One zero-initialized local per object aggregate (and per nested aggregate field type),
/// assigned on entry: zeroing an object is then a copy, which `sroa` splits into constants.
#[derive(Default)]
struct Zeros {
    locals: HashMap<AggId, Local>,
    inits: Vec<Stmt>,
}

impl Zeros {
    /// The zero local of `id`, created (with its entry initialization) on first use.
    fn local(&mut self, aggs: &[AggLayout], func: &mut Function, id: AggId) -> Local {
        if let Some(&l) = self.locals.get(&id) {
            return l;
        }
        let ops: Vec<Operand> = aggs[id.0 as usize]
            .fields
            .iter()
            .map(|&(ty, _)| match ty {
                Ty::Agg(inner) => Operand::Copy(Place::local(self.local(aggs, func, inner))),
                scalar => zero(scalar),
            })
            .collect();
        let l = Local(func.locals.len() as u32);
        func.locals.push(LocalDecl::new(Ty::Agg(id), None));
        self.inits
            .push(Stmt::Assign(Place::local(l), Rvalue::Aggregate(id, ops)));
        self.locals.insert(id, l);
        l
    }

    /// `l = zero` for a new object local, creating the zero local if needed.
    fn assign(&mut self, aggs: &[AggLayout], func: &mut Function, l: Local, obj: AggId) -> Stmt {
        let z = self.local(aggs, func, obj);
        copy(l, z)
    }

    /// `l = zero` once the zero local exists.
    fn fill(&self, l: Local, obj: AggId) -> Stmt {
        copy(l, self.locals[&obj])
    }
}
