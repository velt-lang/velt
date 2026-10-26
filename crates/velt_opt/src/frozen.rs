//! Fields of `frozen` pointees read once (vir.rs invariant 9; docs/internals/design/atomics.md).
//!
//! Nothing changes the pointee of a `frozen` param during the call but the words of its
//! `Atomic`s, which are only accessed through `velt_atomic_*` calls, never read as plain memory:
//! a borrowed object, also one holding atomics (a node with a latch, a pool of counters). So a
//! scalar field the function reads inside a loop is loaded once on entry into a new local, and
//! every read uses the local. The param's `dereferenceable` makes the early load safe on every
//! path, and nothing in between can change the field: no store, call, atomic operation or fence.
//!
//! LLVM cannot do this by itself: an atomic operation stronger than relaxed, a fence, or a call
//! may change any memory it cannot prove private, and a value holding an atomic gets neither
//! `readonly` nor `noalias` (both would be wrong for the atomic's word). Rust's `&T` loses
//! `noalias` the same way once `T` holds an `UnsafeCell`, so a Rust latch read loop reloads the
//! node's array headers and bounds after every acquire; here they stay in registers and the
//! bounds checks leave the loop.
//!
//! A field is left alone when its bytes overlap a place whose address the function takes (an
//! atomic's word, which its operations receive) or that it writes (which `frozen` rules out;
//! kept as a guard), when it lies beyond the `dereferenceable` bytes, and the whole param is
//! left alone when the function assigns it or takes its address.

use velt_vir::vir::{
    AggLayout, Function, Local, LocalDecl, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

use crate::noalias::fresh_entry;
use crate::srclocs::prepend_stmts;
use crate::visit::{places_mut, stmt_places, successors, term_places, PlaceUse};

/// Read the frozen fields of the params of `func` once; returns whether anything changed.
pub(crate) fn run(aggs: &[AggLayout], func: &mut Function) -> bool {
    let mut changed = false;
    for i in 0..func.params.len() {
        let attrs = func.param_attr(i);
        if func.params[i] != Ty::Ptr || !attrs.frozen || attrs.dereferenceable == 0 {
            continue;
        }
        let p = Local(i as u32);
        let names = copies(func, p);
        let fields = scan(aggs, func, &names, p, attrs.dereferenceable);
        if fields.is_empty() {
            continue;
        }
        fresh_entry(func);
        promote(func, &names, p, fields);
        changed = true;
    }
    changed
}

/// A scalar field of the pointee, `(*p as pointee).path`.
struct Field {
    pointee: Ty,
    path: Vec<u32>,
    ty: Ty,
    /// Byte range in the pointee.
    start: u32,
    end: u32,
    /// Read in a block on a cycle.
    in_loop: bool,
}

/// Per local: whether it names `p`'s pointee. `p` itself, and every local other than a param
/// that is only ever assigned a copy of such a local and never has its address taken: the
/// inliner binds a callee's params that way, so an inlined callee's reads count as `p`'s.
/// Computed as the largest such set: start from every candidate and drop the ones with an
/// assignment of another kind until nothing changes.
fn copies(func: &Function, p: Local) -> Vec<bool> {
    let n = func.locals.len();
    let params = func.params.len();
    let mut names: Vec<bool> = (0..n)
        .map(|i| i == p.0 as usize || i >= params && func.locals[i].ty == Ty::Ptr)
        .collect();
    let mut assigned = vec![false; n];
    let mut defs: Vec<(usize, Option<usize>)> = vec![];
    for block in &func.blocks {
        for s in &block.stmts {
            let Stmt::Assign(place, rv) = s else { continue };
            if let Rvalue::AddrOf(a) = rv {
                if a.proj.is_empty() {
                    names[a.local.0 as usize] = false;
                }
            }
            if place.proj.is_empty() {
                let from = match rv {
                    Rvalue::Use(Operand::Copy(src)) if src.proj.is_empty() => {
                        Some(src.local.0 as usize)
                    }
                    _ => None,
                };
                assigned[place.local.0 as usize] = true;
                defs.push((place.local.0 as usize, from));
            }
        }
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            if d.proj.is_empty() {
                names[d.local.0 as usize] = false;
            }
        }
    }
    for (i, a) in assigned.iter().enumerate() {
        if !a && i != p.0 as usize {
            names[i] = false;
        }
    }
    loop {
        let mut changed = false;
        for &(l, from) in &defs {
            if l != p.0 as usize && names[l] && !from.is_some_and(|f| names[f]) {
                names[l] = false;
                changed = true;
            }
        }
        if !changed {
            return names;
        }
    }
}

/// The fields of `*p` worth reading once: scalars read in a loop, within the dereferenceable
/// bytes, overlapping no place whose address is taken or that is written. `names`: the locals
/// that name `p`'s pointee (`copies`).
fn scan(aggs: &[AggLayout], func: &Function, names: &[bool], p: Local, valid: u64) -> Vec<Field> {
    let edges: Vec<Vec<usize>> = func
        .blocks
        .iter()
        .map(|b| successors(&b.term).iter().map(|t| t.0 as usize).collect())
        .collect();
    let in_loop = crate::scc::on_cycle(&edges);
    let mut reads: Vec<Field> = vec![];
    let mut exposed: Vec<(u32, u32)> = vec![];
    let mut whole = false;
    for (bi, block) in func.blocks.iter().enumerate() {
        let mut visit = |pl: &Place, u: PlaceUse| {
            if !names[pl.local.0 as usize] {
                return;
            }
            if pl.proj.is_empty() {
                // `p = …` or `&p`: the param no longer names one pointee (a copy's own
                // assignments are copies, `copies` made sure).
                whole |= pl.local == p && u != PlaceUse::Read;
                return;
            }
            let Some(f) = walk(aggs, pl) else {
                whole = true;
                return;
            };
            let scalar = !matches!(f.ty, Ty::Agg(_));
            let through = f.used < pl.proj.len();
            if scalar
                && (u == PlaceUse::Read || through && matches!(pl.proj[f.used], Proj::Deref(_)))
            {
                // A read of the field, or of the pointer in it, to go through it.
                if let Some(size) = f.ty.scalar_size() {
                    reads.push(Field {
                        pointee: f.pointee,
                        path: f.path,
                        ty: f.ty,
                        start: f.offset,
                        end: f.offset + size,
                        in_loop: in_loop[bi],
                    });
                }
            } else if u != PlaceUse::Read {
                let size = match f.ty {
                    Ty::Agg(a) => aggs.get(a.0 as usize).map_or(u32::MAX, |l| l.size),
                    t => t.scalar_size().unwrap_or(u32::MAX),
                };
                exposed.push((f.offset, f.offset.saturating_add(size)));
            }
        };
        for s in &block.stmts {
            stmt_places(s, &mut visit);
        }
        term_places(&block.term, &mut visit);
    }
    if whole {
        return vec![];
    }
    let mut out: Vec<Field> = vec![];
    for r in reads {
        if u64::from(r.end) > valid || exposed.iter().any(|&(s, e)| r.start < e && s < r.end) {
            continue;
        }
        match out
            .iter_mut()
            .find(|f| f.pointee == r.pointee && f.path == r.path)
        {
            Some(f) => f.in_loop |= r.in_loop,
            None => out.push(r),
        }
    }
    out.retain(|f| f.in_loop);
    out
}

/// `(*p as T).f.g…` as far as its projections are fields.
struct Walk {
    pointee: Ty,
    path: Vec<u32>,
    /// The type reached and its byte offset in `T`.
    ty: Ty,
    offset: u32,
    /// Projections walked (the deref and the fields).
    used: usize,
}

fn walk(aggs: &[AggLayout], pl: &Place) -> Option<Walk> {
    let Some(Proj::Deref(pointee)) = pl.proj.first() else {
        return None;
    };
    let (mut ty, mut offset, mut path) = (*pointee, 0u32, vec![]);
    for x in &pl.proj[1..] {
        let (Proj::Field(n), Ty::Agg(a)) = (x, ty) else {
            break;
        };
        let &(t, o) = aggs.get(a.0 as usize)?.fields.get(*n as usize)?;
        ty = t;
        offset = offset.checked_add(o)?;
        path.push(*n);
    }
    let used = path.len() + 1;
    Some(Walk {
        pointee: *pointee,
        path,
        ty,
        offset,
        used,
    })
}

fn promote(func: &mut Function, names: &[bool], p: Local, fields: Vec<Field>) {
    let mut locals = vec![];
    for f in &fields {
        func.locals.push(LocalDecl::new(f.ty, None));
        locals.push(Local(func.locals.len() as u32 - 1));
    }
    // Rewrite the reads first: the loads added below must keep naming `*p`.
    places_mut(func, &mut |pl| {
        if !names.get(pl.local.0 as usize).copied().unwrap_or(false) {
            return;
        }
        let hit = fields.iter().position(|f| {
            pl.proj.len() > f.path.len()
                && pl.proj[0] == Proj::Deref(f.pointee)
                && pl.proj[1..=f.path.len()]
                    .iter()
                    .zip(&f.path)
                    .all(|(x, n)| *x == Proj::Field(*n))
        });
        if let Some(i) = hit {
            let rest = pl.proj.split_off(fields[i].path.len() + 1);
            *pl = Place {
                local: locals[i],
                proj: rest,
            };
        }
    });
    let loads = fields
        .iter()
        .zip(&locals)
        .map(|(f, &l)| {
            let mut proj = vec![Proj::Deref(f.pointee)];
            proj.extend(f.path.iter().map(|n| Proj::Field(*n)));
            let from = Place { local: p, proj };
            Stmt::Assign(Place::local(l), Rvalue::Use(Operand::Copy(from)))
        })
        .collect();
    prepend_stmts(func, 0, loads);
}

#[cfg(test)]
mod tests;
