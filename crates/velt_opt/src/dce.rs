//! Dead store and dead local elimination.
//!
//! A *removable store* is an assignment into a local that is not address-taken, not through
//! a pointer (whole local or a field of a non-escaping aggregate). Rvalues have no side
//! effects in VIR, so such a store only matters if its local is read. Liveness is computed
//! mark-and-sweep: locals read by essential code (other statements, terminators, calls) are
//! live, and the stores into a live local make their operands live. Calls are always kept
//! (their destinations too, since the callee runs anyway). Finally locals that no longer
//! appear anywhere are deleted and the rest renumbered.
//!
//! Taking a local's address counts as assigning it (vir.rs invariant 6: an out-pointer callee
//! writes through it). Once inlining and `addr_forward` turned the writes through such a pointer
//! into direct writes, the address itself is often dead, but the direct writes may sit on only
//! some paths (a callee that writes its out-pointer only on success, read by the caller only on
//! success). Removing the dead `&x` would leave `x` read before it is (syntactically) assigned,
//! so a still-read `x` gets a zero initialization in the entry block instead.
//!
//! A local holding a source variable for debuggers (`LocalDecl::debug`, only in debug builds)
//! is live: a debugger reads it.

use velt_vir::vir::{AggLayout, Function, Local, Place, Proj, Rvalue, Stmt, Ty};

use crate::locals::Usage;
use crate::srclocs::{prepend_stmts, retain_stmts};
use crate::visit::{derefs, places_mut, stmt_places, term_places, PlaceUse};

/// Remove dead stores and unused locals; returns whether anything changed.
pub(crate) fn run(aggs: &[AggLayout], func: &mut Function) -> bool {
    let mut changed = remove_nops(func);
    changed |= remove_dead_stores(aggs, func);
    changed |= remove_unused_locals(func);
    changed
}

fn remove_nops(func: &mut Function) -> bool {
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        changed |= retain_stmts(func, bi, |s| match s {
            Stmt::Nop => false,
            Stmt::Assign(p, Rvalue::Use(velt_vir::vir::Operand::Copy(q))) => p != q,
            _ => true,
        });
    }
    changed
}

/// The local a removable store writes, if `s` is one.
fn store_target(usage: &Usage, s: &Stmt) -> Option<Local> {
    match s {
        Stmt::Assign(p, _) if !derefs(p) && !usage.get(p.local).address_taken => Some(p.local),
        _ => None,
    }
}

/// Locals read by `s` (as operands, pointer bases, or through `AddrOf(*p…)`).
fn reads(s: &Stmt, out: &mut Vec<Local>) {
    stmt_places(s, &mut |p: &Place, use_| {
        if use_ == PlaceUse::Read || derefs(p) {
            out.push(p.local);
        }
    });
}

/// Mark `l` live, queueing it the first time.
fn mark(l: Local, live: &mut [bool], work: &mut Vec<Local>) {
    if !std::mem::replace(&mut live[l.0 as usize], true) {
        work.push(l);
    }
}

fn remove_dead_stores(aggs: &[AggLayout], func: &mut Function) -> bool {
    let usage = Usage::of(func);
    let n = func.locals.len();
    let mut live = vec![false; n];
    let mut work = Vec::new();
    let mut stores: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n];
    let mut scratch = Vec::new();
    for (i, l) in func.locals.iter().enumerate() {
        if l.debug.is_some() {
            mark(Local(i as u32), &mut live, &mut work);
        }
    }
    for (bi, block) in func.blocks.iter().enumerate() {
        for (si, s) in block.stmts.iter().enumerate() {
            match store_target(&usage, s) {
                Some(l) => stores[l.0 as usize].push((bi, si)),
                None => {
                    reads(s, &mut scratch);
                    scratch
                        .drain(..)
                        .for_each(|l| mark(l, &mut live, &mut work));
                }
            }
        }
        term_places(&block.term, &mut |p, use_| {
            if use_ == PlaceUse::Read || derefs(p) {
                scratch.push(p.local);
            }
        });
        scratch
            .drain(..)
            .for_each(|l| mark(l, &mut live, &mut work));
    }
    while let Some(l) = work.pop() {
        for &(bi, si) in &stores[l.0 as usize] {
            reads(&func.blocks[bi].stmts[si], &mut scratch);
            scratch
                .drain(..)
                .for_each(|l| mark(l, &mut live, &mut work));
        }
    }
    let mut changed = false;
    let mut addressed_live: Vec<Local> = vec![];
    for bi in 0..func.blocks.len() {
        changed |= retain_stmts(func, bi, |s| {
            let keep = store_target(&usage, s).is_none_or(|l| live[l.0 as usize]);
            if let (false, Stmt::Assign(_, Rvalue::AddrOf(q))) = (keep, s) {
                if !derefs(q) && live[q.local.0 as usize] && !addressed_live.contains(&q.local) {
                    addressed_live.push(q.local);
                }
            }
            keep
        });
    }
    let params = func.params.len();
    let inits: Vec<Stmt> = addressed_live
        .into_iter()
        .filter(|l| l.0 as usize >= params)
        .filter_map(|l| zero_init(aggs, l, func.locals[l.0 as usize].ty))
        .filter(|s| !func.blocks[0].stmts.contains(s))
        .collect();
    if !inits.is_empty() {
        // After CFG simplification the entry block can be a loop header: the inits must run
        // once, before it, or they would clobber a loop-carried value.
        crate::noalias::fresh_entry(func);
        prepend_stmts(func, 0, inits);
    }
    changed
}

/// A statement that assigns local `l` of type `ty` (for definite assignment): zero for a
/// scalar; for an aggregate, zero into its first non-unit field, innermost (a field write
/// assigns the local), or an empty aggregate. `None` for a value without bytes (`Unit`, or an
/// aggregate of only unit fields): there is nothing to read before it is assigned.
fn zero_init(aggs: &[AggLayout], l: Local, mut ty: Ty) -> Option<Stmt> {
    let mut place = Place::local(l);
    loop {
        match ty {
            Ty::Unit => return None,
            Ty::Agg(id) => {
                let fields = &aggs[id.0 as usize].fields;
                if fields.is_empty() {
                    return Some(Stmt::Assign(place, Rvalue::Aggregate(id, vec![])));
                }
                let (i, &(field, _)) = fields.iter().enumerate().find(|(_, f)| f.0 != Ty::Unit)?;
                place.proj.push(Proj::Field(i as u32));
                ty = field;
            }
            scalar => return Some(Stmt::Assign(place, Rvalue::Use(crate::sroa::zero(scalar)))),
        }
    }
}

fn remove_unused_locals(func: &mut Function) -> bool {
    let n = func.locals.len();
    let mut used = vec![false; n];
    used.iter_mut()
        .take(func.params.len())
        .for_each(|u| *u = true);
    for block in &func.blocks {
        let mut note = |p: &Place, _| used[p.local.0 as usize] = true;
        for s in &block.stmts {
            stmt_places(s, &mut note);
        }
        term_places(&block.term, &mut note);
    }
    if used.iter().all(|&u| u) {
        return false;
    }
    let mut remap = vec![Local(u32::MAX); n];
    let mut next = 0;
    for (i, &u) in used.iter().enumerate() {
        if u {
            remap[i] = Local(next);
            next += 1;
        }
    }
    let mut index = 0;
    func.locals.retain(|_| {
        index += 1;
        used[index - 1]
    });
    places_mut(func, &mut |p| p.local = remap[p.local.0 as usize]);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use velt_vir::vir::{BinOp, BlockId, Callee, ExternId, Terminator, Ty};

    #[test]
    fn removes_dead_chain_and_renumbers_locals() {
        // a = p + 1; b = a * 2 (dead chain); c = p; return c
        let mut fb = FuncBuilder::internal("f", &[Ty::I64], Ty::I64);
        let p = fb.param(0);
        let (a, b, c) = (fb.local(Ty::I64), fb.local(Ty::I64), fb.local(Ty::I64));
        let bb = fb.block();
        fb.assign(bb, a, bin(BinOp::Add, copy_local(p), int(1, Ty::I64)));
        fb.assign(bb, b, bin(BinOp::Mul, copy_local(a), int(2, Ty::I64)));
        fb.assign(bb, c, Rvalue::Use(copy_local(p)));
        fb.ret(bb, copy_local(c));
        let mut f = fb.finish();
        assert!(run(&[], &mut f));
        assert_eq!(f.locals.len(), 2);
        assert_eq!(
            f.blocks[0].stmts,
            vec![Stmt::Assign(
                Place::local(Local(1)),
                Rvalue::Use(copy_local(p))
            )]
        );
        assert_eq!(f.blocks[0].term, Terminator::Return(copy_local(Local(1))));
    }

    #[test]
    fn keeps_escaping_stores_calls_and_pointer_writes() {
        // x = 1; q = &x; *p = 2; call ext(q) -> r (r unused); return 0
        let mut fb = FuncBuilder::internal("f", &[Ty::Ptr], Ty::I64);
        let p = fb.param(0);
        let (x, q, r) = (fb.local(Ty::I64), fb.local(Ty::Ptr), fb.local(Ty::I64));
        let b = fb.block();
        fb.assign(b, x, Rvalue::Use(int(1, Ty::I64)));
        fb.assign(b, q, Rvalue::AddrOf(Place::local(x)));
        fb.push(
            b,
            Stmt::Assign(deref(p, Ty::I64), Rvalue::Use(int(2, Ty::I64))),
        );
        let next = fb.call(b, Callee::Extern(ExternId(0)), vec![copy_local(q)], Some(r));
        fb.ret(next, int(0, Ty::I64));
        let mut f = fb.finish();
        let before = f.blocks[0].stmts.clone();
        run(&[], &mut f);
        assert_eq!(f.blocks[0].stmts, before);
        assert_eq!(f.locals.len(), 4);
    }

    #[test]
    fn loop_carried_but_unobserved_value_is_removed() {
        // i = 0; loop: i = i + 1; goto loop   — i is only read by its own update.
        let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
        let i = fb.local(Ty::I64);
        let (b0, b1) = (fb.block(), fb.block());
        fb.assign(b0, i, Rvalue::Use(int(0, Ty::I64)));
        fb.goto(b0, b1);
        fb.assign(b1, i, bin(BinOp::Add, copy_local(i), int(1, Ty::I64)));
        fb.goto(b1, b1);
        let mut f = fb.finish();
        assert!(run(&[], &mut f));
        assert!(f.blocks.iter().all(|b| b.stmts.is_empty()));
        assert!(f.locals.is_empty());
    }

    #[test]
    fn a_described_variable_is_kept() {
        // x = 1; return 0 — x is never read, but a debugger shows it.
        let mut fb = FuncBuilder::internal("f", &[], Ty::I64);
        let x = fb.local(Ty::I64);
        let b0 = fb.block();
        fb.assign(b0, x, Rvalue::Use(int(1, Ty::I64)));
        fb.ret(b0, int(0, Ty::I64));
        let mut f = fb.finish();
        f.locals[x.0 as usize].debug = Some(velt_vir::vir::LocalDebug {
            decl: velt_vir::vir::SrcLoc {
                file: 0,
                line: 1,
                col: 1,
            },
            ty: velt_vir::vir::DebugTyId(0),
            by_ref: false,
            param: false,
        });
        assert!(!run(&[], &mut f));
        assert_eq!(f.locals.len(), 1);
        assert_eq!(f.blocks[0].stmts.len(), 1);
    }

    /// `q = &x` (dead) keeps `x` assigned for the verifier; `x` (or its field 1, for an
    /// aggregate) is written only when `c` holds and read only when `c` holds again.
    fn addressed_on_one_path(ty: Ty) -> Function {
        let mut fb = FuncBuilder::internal("f", &[Ty::Bool], Ty::I64);
        let c = fb.param(0);
        let (x, q) = (fb.local(ty), fb.local(Ty::Ptr));
        let bbs: Vec<BlockId> = (0..6).map(|_| fb.block()).collect();
        let target = match ty {
            Ty::Agg(_) => field(x, 1),
            _ => Place::local(x),
        };
        fb.assign(bbs[0], q, Rvalue::AddrOf(Place::local(x)));
        fb.branch(bbs[0], c, bbs[1], bbs[2]);
        fb.push(
            bbs[1],
            Stmt::Assign(target.clone(), Rvalue::Use(int(5, Ty::I64))),
        );
        fb.goto(bbs[1], bbs[3]);
        fb.goto(bbs[2], bbs[3]);
        fb.branch(bbs[3], c, bbs[4], bbs[5]);
        fb.ret(bbs[4], copy_place(target));
        fb.ret(bbs[5], int(0, Ty::I64));
        fb.finish()
    }

    /// Runs dce on `f` (in `pb`'s program): the `&x` goes, the program still verifies.
    fn removes_address_and_verifies(mut pb: ProgramBuilder, mut f: Function) {
        assert!(run(&pb.p.aggs, &mut f));
        assert!(!f
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .any(|s| matches!(s, Stmt::Assign(_, Rvalue::AddrOf(_)))));
        pb.add(f);
        let p = pb.finish();
        assert_eq!(crate::testkit::validate::validate(&p), Ok(()), "{p}");
    }

    #[test]
    fn dead_address_of_a_read_scalar_becomes_a_zero_init() {
        removes_address_and_verifies(ProgramBuilder::new(), addressed_on_one_path(Ty::I64));
    }

    #[test]
    fn dead_address_of_a_read_aggregate_becomes_a_field_init() {
        // outer { pair { ptr, i64 }, i64 }: the init writes the innermost first field.
        let mut pb = ProgramBuilder::new();
        let pair = pb.agg("pair", 16, 8, &[(Ty::Ptr, 0), (Ty::I64, 8)]);
        let outer = pb.agg("outer", 24, 8, &[(Ty::Agg(pair), 0), (Ty::I64, 16)]);
        removes_address_and_verifies(pb, addressed_on_one_path(Ty::Agg(outer)));
    }

    #[test]
    fn aggregate_init_skips_unit_fields() {
        // { unit, i64 }: the init writes field 1.
        let mut pb = ProgramBuilder::new();
        let agg = pb.agg("u", 8, 8, &[(Ty::Unit, 0), (Ty::I64, 0)]);
        let s = zero_init(&pb.p.aggs, Local(0), Ty::Agg(agg));
        assert_eq!(
            s,
            Some(Stmt::Assign(
                field(Local(0), 1),
                Rvalue::Use(int(0, Ty::I64))
            ))
        );
        let only_unit = pb.agg("v", 0, 1, &[(Ty::Unit, 0)]);
        assert_eq!(zero_init(&pb.p.aggs, Local(0), Ty::Agg(only_unit)), None);
    }

    #[test]
    fn zero_init_of_a_loop_header_entry_goes_in_a_fresh_entry() {
        // bb0 (loop header): q = &x; if c { x = 5 }; if c { return x } else goto bb0.
        let mut fb = FuncBuilder::internal("f", &[Ty::Bool], Ty::I64);
        let c = fb.param(0);
        let (x, q) = (fb.local(Ty::I64), fb.local(Ty::Ptr));
        let bbs: Vec<BlockId> = (0..4).map(|_| fb.block()).collect();
        fb.assign(bbs[0], q, Rvalue::AddrOf(Place::local(x)));
        fb.branch(bbs[0], c, bbs[1], bbs[2]);
        fb.assign(bbs[1], x, Rvalue::Use(int(5, Ty::I64)));
        fb.goto(bbs[1], bbs[2]);
        fb.branch(bbs[2], c, bbs[3], bbs[0]);
        fb.ret(bbs[3], copy_local(x));
        let mut f = fb.finish();
        assert!(run(&[], &mut f));
        // The init runs once, in a new entry that jumps to the old one (now the last block).
        let header = BlockId(f.blocks.len() as u32 - 1);
        assert_eq!(f.blocks[0].term, Terminator::Goto(header));
        assert!(matches!(
            &f.blocks[0].stmts[..],
            [Stmt::Assign(_, Rvalue::Use(_))]
        ));
        assert!(f.blocks[header.0 as usize].stmts.is_empty());
        assert_eq!(
            f.blocks[2].term,
            Terminator::Branch {
                cond: copy_local(Local(0)),
                then: BlockId(3),
                els: header,
            }
        );
        let mut pb = ProgramBuilder::new();
        pb.add(f);
        let p = pb.finish();
        assert_eq!(crate::testkit::validate::validate(&p), Ok(()), "{p}");
    }
}
