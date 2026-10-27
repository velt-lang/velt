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
//! A field is left alone when its bytes overlap a place the function writes (which `frozen`
//! rules out; kept as a guard) and when it lies beyond the `dereferenceable` bytes, and the
//! whole param is left alone when the function assigns it or takes its address. A field whose
//! address is taken is still read once: `frozen` holds through every pointer, and an atomic's
//! word, the one part that changes, is only reached through `velt_atomic_*` calls.

use velt_vir::vir::{
    AggLayout, Function, Local, LocalDecl, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

use crate::noalias::fresh_entry;
use crate::srclocs::prepend_stmts;
use crate::visit::{
    stmt_operands_mut, stmt_places, successors, term_operands_mut, term_places, PlaceUse,
};

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
        promote(aggs, func, &names, p, fields);
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

/// Per local: what part of `p`'s pointee it points to, if it is one of `p`'s names. `p` itself,
/// and every local other than a param that is only ever assigned a copy of such a local, or the
/// address of a part of its pointee (`q = &(*p as T).f.g`: the projections `[Deref(T), Field(f),
/// Field(g)]`), and never has its own address taken: the inliner binds a callee's params that
/// way, and lowering passes an array field to a function by its address, so those reads count as
/// `p`'s. Computed as the largest such set: start from every candidate and drop the ones with an
/// assignment of another kind until nothing changes.
fn copies(func: &Function, p: Local) -> Vec<Option<Vec<Proj>>> {
    let n = func.locals.len();
    let params = func.params.len();
    let mut cand: Vec<bool> = (0..n)
        .map(|i| i == p.0 as usize || i >= params && func.locals[i].ty == Ty::Ptr)
        .collect();
    let mut assigned = vec![false; n];
    // Whole-local assignments: (local, source local, the source's projections taken the
    // address of: empty for a copy).
    let mut defs: Vec<(usize, Option<Source>)> = vec![];
    for block in &func.blocks {
        for s in &block.stmts {
            let Stmt::Assign(place, rv) = s else { continue };
            if let Rvalue::AddrOf(a) = rv {
                if a.proj.is_empty() {
                    cand[a.local.0 as usize] = false;
                }
            }
            if place.proj.is_empty() {
                let from = match rv {
                    Rvalue::Use(Operand::Copy(src)) if src.proj.is_empty() => {
                        Some((src.local.0 as usize, vec![]))
                    }
                    Rvalue::AddrOf(src)
                        if matches!(src.proj.first(), Some(Proj::Deref(_)))
                            && src.proj[1..].iter().all(|x| matches!(x, Proj::Field(_))) =>
                    {
                        Some((src.local.0 as usize, src.proj.clone()))
                    }
                    _ => None,
                };
                assigned[place.local.0 as usize] = true;
                defs.push((place.local.0 as usize, from));
            }
        }
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            if d.proj.is_empty() {
                cand[d.local.0 as usize] = false;
            }
        }
    }
    for (i, a) in assigned.iter().enumerate() {
        if !a && i != p.0 as usize {
            cand[i] = false;
        }
    }
    // Each candidate's prefix: every assignment of it must give the same one.
    let mut names: Vec<Option<Vec<Proj>>> = vec![None; n];
    names[p.0 as usize] = Some(vec![]);
    loop {
        let mut changed = false;
        for (l, from) in &defs {
            let l = *l;
            if l == p.0 as usize || !cand[l] {
                continue;
            }
            let got = from.as_ref().and_then(|(src, proj)| {
                let base = names[*src].as_ref()?;
                if proj.is_empty() {
                    return Some(base.clone());
                }
                // `&(*src as T).f…`: only through a plain name of `p` (a prefix of a prefix
                // would need its types checked twice).
                base.is_empty().then(|| proj.clone())
            });
            match (got, &names[l]) {
                (Some(g), None) => {
                    names[l] = Some(g);
                    changed = true;
                }
                (Some(g), Some(cur)) if g == *cur => {}
                _ => {
                    // Another kind of assignment, or two different parts: not a name.
                    if names[l].is_some() || cand[l] {
                        names[l] = None;
                        cand[l] = false;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return names;
        }
    }
}

/// A name's source: the local it copies or takes an address in, and the projections of that
/// address (empty for a copy).
type Source = (usize, Vec<Proj>);

/// The place `pl` (of a name of `p`) as a place of `p` itself: `(*q as U).rest` with
/// `q = &(*p as T).f.g` is `(*p as T).f.g.rest` when `U` is the type at `T.f.g`.
fn canon(aggs: &[AggLayout], names: &[Option<Vec<Proj>>], p: Local, pl: &Place) -> Option<Place> {
    let prefix = names.get(pl.local.0 as usize)?.as_ref()?;
    if prefix.is_empty() {
        return Some(Place {
            local: p,
            proj: pl.proj.clone(),
        });
    }
    let Some(Proj::Deref(u)) = pl.proj.first().cloned() else {
        return None;
    };
    let Some(Proj::Deref(mut ty)) = prefix.first().cloned() else {
        return None;
    };
    for x in &prefix[1..] {
        let (Proj::Field(f), Ty::Agg(a)) = (x, ty) else {
            return None;
        };
        ty = aggs.get(a.0 as usize)?.fields.get(*f as usize)?.0;
    }
    if ty != u {
        return None;
    }
    let mut proj = prefix.clone();
    proj.extend(pl.proj[1..].iter().cloned());
    Some(Place { local: p, proj })
}

/// The fields of `*p` worth reading once: scalars read in a loop, within the dereferenceable
/// bytes, overlapping no place whose address is taken or that is written. `names`: the locals
/// that name `p`'s pointee (`copies`).
fn scan(
    aggs: &[AggLayout],
    func: &Function,
    names: &[Option<Vec<Proj>>],
    p: Local,
    valid: u64,
) -> Vec<Field> {
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
            if names.get(pl.local.0 as usize).is_none_or(|n| n.is_none()) {
                return;
            }
            if pl.proj.is_empty() {
                // `p = …` or `&p`: the param no longer names one pointee (a name's own
                // assignments are copies and addresses, `copies` made sure).
                whole |= pl.local == p && u != PlaceUse::Read;
                return;
            }
            let Some(pl) = canon(aggs, names, p, pl) else {
                whole = true;
                return;
            };
            let pl = &pl;
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
            } else if u == PlaceUse::Write {
                // A taken address does not count: nothing changes the pointee's bytes through
                // it either (`frozen`), and the atomic words it may lead to are never read as
                // plain memory. Passing an array field to a function that reads it (a SIMD
                // load of an arena) keeps its header promotable.
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

fn promote(
    aggs: &[AggLayout],
    func: &mut Function,
    names: &[Option<Vec<Proj>>],
    p: Local,
    fields: Vec<Field>,
) {
    let mut locals = vec![];
    for f in &fields {
        func.locals.push(LocalDecl::new(f.ty, None));
        locals.push(Local(func.locals.len() as u32 - 1));
    }
    // Rewrite the reads first: the loads added below must keep naming `*p`.
    let rewrite = |pl: &mut Place, addr: bool| {
        if pl.proj.is_empty() {
            return;
        }
        let Some(c) = canon(aggs, names, p, pl) else {
            return;
        };
        *pl = c;
        let hit = fields.iter().position(|f| {
            pl.proj.len() > f.path.len()
                && pl.proj[0] == Proj::Deref(f.pointee)
                && pl.proj[1..=f.path.len()]
                    .iter()
                    .zip(&f.path)
                    .all(|(x, n)| *x == Proj::Field(*n))
        });
        if let Some(i) = hit {
            // The address of the field itself stays the field's: only a place through the
            // pointer the field holds may start from the local.
            if addr && pl.proj.len() == fields[i].path.len() + 1 {
                return;
            }
            let rest = pl.proj.split_off(fields[i].path.len() + 1);
            *pl = Place {
                local: locals[i],
                proj: rest,
            };
        }
    };
    for block in &mut func.blocks {
        for s in &mut block.stmts {
            stmt_operands_mut(s, &mut |op| {
                if let Operand::Copy(pl) = op {
                    rewrite(pl, false);
                }
            });
            if let Stmt::Assign(dst, rv) = s {
                if let Rvalue::AddrOf(pl) = rv {
                    rewrite(pl, true);
                }
                rewrite(dst, false);
            }
        }
        term_operands_mut(&mut block.term, &mut |op| {
            if let Operand::Copy(pl) = op {
                rewrite(pl, false);
            }
        });
        if let Terminator::Call { dest: Some(d), .. } = &mut block.term {
            rewrite(d, false);
        }
    }
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
