//! Tags decided by the edge into their test: a jump into a block that only tests a field of a
//! local aggregate (`res.0 != 0`, the tag of a callee's `Result` after it was inlined) goes to
//! the test's target directly when the jumping block has just stored a constant there.
//!
//! An inlined throwing callee stores its result's tag (`res.0 = 1` where it throws, `= 0` where
//! it returns) and jumps to the caller's check of it, a join of every return of the callee, so
//! the error path and the Ok path run through one test that nothing proves. With the jumps
//! threaded, each path continues where its tag sends it: the code after an inlined `corrupt(…)`
//! is the error path only, as it is after a call (`Function::cold`), and the test goes once
//! every path is threaded.
//!
//! The test block holds `x = l.f…` and `c = x == k` (or `!=`) and branches on `c`, or switches
//! on `x`, where `l` is an aggregate local whose address is never taken and `x` and `c` are read
//! nowhere else. The jumping block's last write to `l` that overlaps the tested bytes must be a
//! constant store to exactly that place.

use velt_vir::vir::{
    AggLayout, BinOp, BlockId, Const, Function, Local, Operand, Place, Proj, Rvalue, Stmt,
    Terminator, Ty,
};

use crate::locals::{place_ty, Usage};

/// Thread every jump into a tag test that the jumping block decides; returns whether any was.
pub(crate) fn run(aggs: &[AggLayout], func: &mut Function) -> bool {
    let usage = Usage::of(func);
    let mut changed = false;
    for p in 0..func.blocks.len() {
        let Terminator::Goto(b) = func.blocks[p].term else {
            continue;
        };
        let Some(test) = tag_test(func, &usage, b) else {
            continue;
        };
        let Some(v) = stored_constant(aggs, func, p, &test.place) else {
            continue;
        };
        func.blocks[p].term = Terminator::Goto(test.target(v));
        changed = true;
    }
    changed
}

/// A block that tests `place` (`l.f…`) against constants.
struct TagTest {
    place: Place,
    /// `x == k`, `x != k` (with the branch's targets), or a switch on `x`.
    kind: Kind,
}

enum Kind {
    Compare {
        eq: bool,
        k: i128,
        then: BlockId,
        els: BlockId,
    },
    Switch {
        cases: Vec<(i128, BlockId)>,
        default: BlockId,
    },
}

impl TagTest {
    fn target(&self, v: i128) -> BlockId {
        match &self.kind {
            Kind::Compare { eq, k, then, els } => {
                if (v == *k) == *eq {
                    *then
                } else {
                    *els
                }
            }
            Kind::Switch { cases, default } => cases
                .iter()
                .find(|(c, _)| *c == v)
                .map_or(*default, |(_, b)| *b),
        }
    }
}

/// Block `b` as a tag test (module doc).
fn tag_test(func: &Function, usage: &Usage, b: BlockId) -> Option<TagTest> {
    let block = &func.blocks[b.0 as usize];
    let read_once = |l: Local| usage.is_register(l) && usage.get(l).reads == 1;
    let (x, place) = match block.stmts.first()? {
        Stmt::Assign(d, Rvalue::Use(Operand::Copy(src))) if d.proj.is_empty() => {
            (d.local, src.clone())
        }
        _ => return None,
    };
    let local_agg = !place.proj.is_empty()
        && place
            .proj
            .iter()
            .all(|p| matches!(p, Proj::Field(_) | Proj::Cast(_)))
        && !usage.get(place.local).address_taken
        && !usage.is_register(place.local);
    if !local_agg || !read_once(x) {
        return None;
    }
    let kind = match (&block.stmts[1..], &block.term) {
        (
            [],
            Terminator::Switch {
                value,
                cases,
                default,
            },
        ) if is_local(value, x) => Kind::Switch {
            cases: cases.clone(),
            default: *default,
        },
        (
            [Stmt::Assign(c, Rvalue::Binary(op @ (BinOp::Eq | BinOp::Ne), l, r))],
            Terminator::Branch { cond, then, els },
        ) if c.proj.is_empty() && is_local(cond, c.local) && read_once(c.local) => {
            let k = match (l, r) {
                (a, Operand::Const(Const::Int(k), _)) | (Operand::Const(Const::Int(k), _), a)
                    if is_local(a, x) =>
                {
                    *k
                }
                _ => return None,
            };
            Kind::Compare {
                eq: *op == BinOp::Eq,
                k,
                then: *then,
                els: *els,
            }
        }
        _ => return None,
    };
    Some(TagTest { place, kind })
}

fn is_local(op: &Operand, l: Local) -> bool {
    matches!(op, Operand::Copy(p) if p.proj.is_empty() && p.local == l)
}

/// The constant block `p` leaves in `place`: its last write to the place's local that overlaps
/// it stores a constant to exactly it.
fn stored_constant(aggs: &[AggLayout], func: &Function, p: usize, place: &Place) -> Option<i128> {
    let (start, size) = byte_range(aggs, func, place)?;
    // Memory operations only reach locals whose address is taken; `place`'s is not.
    for s in func.blocks[p].stmts.iter().rev() {
        let Stmt::Assign(d, rv) = s else { continue };
        if d.local != place.local {
            continue;
        }
        if d == place {
            return match rv {
                Rvalue::Use(Operand::Const(Const::Int(v), _)) => Some(*v),
                Rvalue::Use(Operand::Const(Const::Bool(v), _)) => Some(i128::from(*v)),
                _ => None,
            };
        }
        let (s2, n2) = byte_range(aggs, func, d)?;
        if s2 < start + size && start < s2 + n2 {
            return None;
        }
    }
    None
}

/// The bytes of `place` (a local's field path, no dereference) within the local.
fn byte_range(aggs: &[AggLayout], func: &Function, place: &Place) -> Option<(u64, u64)> {
    let mut ty = func.locals.get(place.local.0 as usize)?.ty;
    let mut start = 0u64;
    for proj in &place.proj {
        ty = match (proj, ty) {
            (Proj::Field(n), Ty::Agg(id)) => {
                let (fty, off) = *aggs.get(id.0 as usize)?.fields.get(*n as usize)?;
                start += u64::from(off);
                fty
            }
            // A view of the same bytes.
            (Proj::Cast(id), _) => Ty::Agg(*id),
            _ => return None,
        };
    }
    let size = match ty {
        Ty::Agg(id) => u64::from(aggs.get(id.0 as usize)?.size),
        t => u64::from(t.scalar_size()?),
    };
    debug_assert_eq!(place_ty(aggs, func, place), Some(ty));
    Some((start, size))
}

#[cfg(test)]
mod tests;
