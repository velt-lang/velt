//! Int32 representation of JS numbers, first slice of `numrep` (design #525, step 1).
//!
//! The results of JS's 32-bit operators (`x | 0`, `x >>> 0`, `Math.imul`) are 32-bit integers,
//! but a program keeps them in `number` (`f64`) or inferred-integer (`i64`) variables. This pass
//! stores such values as `i32`:
//! - **Narrowing**: an `f64` or `i64` register local whose every definition is a 32-bit integer
//!   value (a conversion of an `i32`, `i16`, `i8`, `u16` or `u8` value or of another such local,
//!   an integral constant in range, or `0.0 + v` of one) becomes an `i32` local. Each read
//!   converts it back with a `Cast` just before the use. Converting an int32 to `f64` or `i64` is
//!   exact and never gives `-0`, so every read sees the value it saw before, while the value
//!   carried around a loop now lives in an integer register and ToInt32 of it folds away.
//! - **Int32 sums**: `velt_rt_math_to_int32(a ± b)`, where `a` and `b` are `f64` conversions of
//!   `i32` values made in the same block, is their wrapping 32-bit sum or difference: the double
//!   operation is exact, because |a ± b| < 2^32.
//! - **Sums with converted integers**: `velt_rt_math_add_int32(a, x)` (ToInt32 of `a + x`) where
//!   `x` converts an `i64` `c` in the same block (`(y + i) | 0` once a counter is passed as a
//!   `number` and inlined) adds as integers when |c| <= 2^52, the double sum being exact then;
//!   other values keep the call. That skips converting the counter back from the double.

use velt_vir::vir::{
    BinOp, Callee, Const, ExternFn, Function, Local, LocalDecl, Operand, Place, Rvalue, Stmt,
    Terminator, Ty,
};

use crate::locals::Usage;
use crate::srclocs::{push_stmt, rewrite_stmts};
use crate::visit::{stmt_operands_mut, term_operands_mut};

/// The runtime's ToInt32 (`velt_rt_math_to_int32(f64) -> i32`).
const TO_INT32: &str = "velt_rt_math_to_int32";
/// ToInt32 of `a + x` for an int32 `a` (`velt_rt_math_add_int32(i32, f64) -> i32`).
const ADD_INT32: &str = "velt_rt_math_add_int32";

/// Narrow the int32-valued locals of `func` and fold the int32 sums; returns whether anything
/// changed.
pub(crate) fn run(externs: &[ExternFn], func: &mut Function) -> bool {
    let narrowed = narrow(func);
    let summed = int32_sums(externs, func);
    let counted = converted_sums(externs, func);
    narrowed || summed || counted
}

fn small_int(ty: Ty) -> bool {
    matches!(ty, Ty::I32 | Ty::I16 | Ty::I8 | Ty::U16 | Ty::U8)
}

fn int32_const(c: &Const, ty: Ty) -> Option<i128> {
    match (c, ty) {
        (Const::Int(n), t) if t.is_int() && i32::try_from(*n).is_ok() => Some(*n),
        (Const::Float(f), Ty::F64)
            if f.fract() == 0.0
                && *f >= i32::MIN as f64
                && *f <= i32::MAX as f64
                && !(*f == 0.0 && f.is_sign_negative()) =>
        {
            Some(*f as i128)
        }
        _ => None,
    }
}

/// Facts about candidate locals while narrowing.
struct Narrowing<'f> {
    func: &'f Function,
    /// Indexed by local: still an int32-valued candidate.
    keep: Vec<bool>,
}

impl Narrowing<'_> {
    fn local_ty(&self, l: Local) -> Ty {
        self.func.locals[l.0 as usize].ty
    }

    /// An operand whose value is a 32-bit integer.
    fn source(&self, op: &Operand) -> bool {
        match op {
            Operand::Const(c, ty) => int32_const(c, *ty).is_some(),
            Operand::Copy(p) if p.proj.is_empty() => {
                small_int(self.local_ty(p.local)) || self.keep[p.local.0 as usize]
            }
            Operand::Copy(_) => false,
        }
    }

    /// The int32 operand an int32-valued definition copies or converts.
    fn def_source<'r>(&self, dst: Ty, rv: &'r Rvalue) -> Option<&'r Operand> {
        let op = match rv {
            Rvalue::Use(op) | Rvalue::Cast(op, _) => op,
            Rvalue::Binary(BinOp::Add, a, b) if dst == Ty::F64 => match (a, b) {
                (Operand::Const(Const::Float(z), Ty::F64), x)
                | (x, Operand::Const(Const::Float(z), Ty::F64))
                    if *z == 0.0 && !z.is_sign_negative() =>
                {
                    x
                }
                _ => return None,
            },
            _ => return None,
        };
        self.source(op).then_some(op)
    }
}

/// The locals `narrow` may rewrite: non-param `f64`/`i64` register locals that are assigned only
/// as a whole by statements (not by calls).
fn candidates(func: &Function) -> Vec<bool> {
    let usage = Usage::of(func);
    let mut keep: Vec<bool> = (0..func.locals.len())
        .map(|i| {
            let l = Local(i as u32);
            let u = usage.get(l);
            i >= func.params.len()
                && matches!(func.locals[i].ty, Ty::F64 | Ty::I64)
                && usage.is_register(l)
                && u.defs > 0
                && u.partial_defs == 0
        })
        .collect();
    for b in &func.blocks {
        if let Terminator::Call { dest: Some(d), .. } = &b.term {
            keep[d.local.0 as usize] = false;
        }
    }
    keep
}

fn narrow(func: &mut Function) -> bool {
    let mut n = Narrowing {
        func,
        keep: candidates(func),
    };
    // Greatest fixpoint: drop every candidate with a definition that is not int32-valued.
    loop {
        let mut changed = false;
        for s in n.func.blocks.iter().flat_map(|b| &b.stmts) {
            let Stmt::Assign(p, rv) = s else { continue };
            let i = p.local.0 as usize;
            if p.proj.is_empty() && n.keep[i] && n.def_source(n.func.locals[i].ty, rv).is_none() {
                n.keep[i] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let keep = n.keep;
    if !keep.iter().any(|&k| k) {
        return false;
    }
    let mut map: Vec<Option<Local>> = vec![None; func.locals.len()];
    for (i, &k) in keep.iter().enumerate() {
        if k {
            map[i] = Some(Local(func.locals.len() as u32));
            let name = func.locals[i].name.clone();
            func.locals.push(LocalDecl { ty: Ty::I32, name });
        }
    }
    let tys: Vec<Ty> = func.locals.iter().map(|l| l.ty).collect();
    let mut temps: Vec<Ty> = vec![];
    let base = func.locals.len();
    for bi in 0..func.blocks.len() {
        rewrite_stmts(func, bi, |s, out| match s {
            Stmt::Assign(p, rv) if p.proj.is_empty() && map[p.local.0 as usize].is_some() => {
                let to = map[p.local.0 as usize].unwrap_or(p.local);
                out.push(Stmt::Assign(Place::local(to), narrowed_def(&rv, &map, &tys)));
            }
            mut s => {
                stmt_operands_mut(&mut s, &mut |op| {
                    widen_read(op, &map, &tys, base, &mut temps, out)
                });
                out.push(s);
            }
        });
        let mut term = std::mem::replace(&mut func.blocks[bi].term, Terminator::Unreachable);
        let mut casts = vec![];
        term_operands_mut(&mut term, &mut |op| {
            widen_read(op, &map, &tys, base, &mut temps, &mut casts)
        });
        for c in casts {
            push_stmt(func, bi, c, None);
        }
        func.blocks[bi].term = term;
    }
    func.locals.extend(temps.into_iter().map(|ty| LocalDecl { ty, name: None }));
    true
}

/// The `i32` rvalue of a narrowed local's definition (`Narrowing::def_source` held for it).
fn narrowed_def(rv: &Rvalue, map: &[Option<Local>], tys: &[Ty]) -> Rvalue {
    let op = match rv {
        Rvalue::Use(op) | Rvalue::Cast(op, _) => op,
        Rvalue::Binary(_, Operand::Const(Const::Float(_), _), x) => x,
        Rvalue::Binary(_, x, _) => x,
        _ => panic!("ICE: numrep narrowed a local with a non-int32 definition"),
    };
    match op {
        Operand::Const(c, ty) => {
            let v = int32_const(c, *ty).expect("ICE: numrep int32 constant");
            Rvalue::Use(Operand::Const(Const::Int(v), Ty::I32))
        }
        Operand::Copy(p) => match map[p.local.0 as usize] {
            Some(m) => Rvalue::Use(Operand::Copy(Place::local(m))),
            None if tys[p.local.0 as usize] == Ty::I32 => Rvalue::Use(op.clone()),
            None => Rvalue::Cast(op.clone(), Ty::I32),
        },
    }
}

/// A read of a narrowed local becomes a read of a fresh temp converted from its `i32` (the
/// conversion is pushed to `out`, before the statement that reads it).
fn widen_read(
    op: &mut Operand,
    map: &[Option<Local>],
    tys: &[Ty],
    base: usize,
    temps: &mut Vec<Ty>,
    out: &mut Vec<Stmt>,
) {
    let Operand::Copy(p) = op else { return };
    if !p.proj.is_empty() {
        return;
    }
    let Some(m) = map.get(p.local.0 as usize).copied().flatten() else {
        return;
    };
    let ty = tys[p.local.0 as usize];
    let t = Local((base + temps.len()) as u32);
    temps.push(ty);
    out.push(Stmt::Assign(
        Place::local(t),
        Rvalue::Cast(Operand::Copy(Place::local(m)), ty),
    ));
    *op = Operand::Copy(Place::local(t));
}

/// `velt_rt_math_to_int32(a ± b)` of two converted `i32` values → their wrapping `i32` sum.
fn int32_sums(externs: &[ExternFn], func: &mut Function) -> bool {
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        let Some((op, a, b, dest, next)) = int32_sum(externs, func, bi) else {
            continue;
        };
        if let Some(d) = dest {
            push_stmt(func, bi, Stmt::Assign(d, Rvalue::Binary(op, a, b)), None);
        }
        func.blocks[bi].term = Terminator::Goto(next);
        changed = true;
    }
    changed
}

type Sum = (BinOp, Operand, Operand, Option<Place>, velt_vir::vir::BlockId);

fn int32_sum(externs: &[ExternFn], func: &Function, bi: usize) -> Option<Sum> {
    let block = &func.blocks[bi];
    let Terminator::Call {
        callee: Callee::Extern(id),
        args,
        dest,
        next,
    } = &block.term
    else {
        return None;
    };
    if externs.get(id.0 as usize)?.symbol != TO_INT32 {
        return None;
    }
    let [Operand::Copy(t)] = args.as_slice() else {
        return None;
    };
    let end = block.stmts.len();
    let (i, rv) = last_def(&block.stmts, t, end)?;
    let Rvalue::Binary(op @ (BinOp::Add | BinOp::Sub), x, y) = rv else {
        return None;
    };
    let a = int32_operand(func, &block.stmts, x, i)?;
    let b = int32_operand(func, &block.stmts, y, i)?;
    // The operands must still hold their values at the call.
    let unchanged = |o: &Operand| match o {
        Operand::Copy(p) => !assigned(&block.stmts[i..], p.local),
        Operand::Const(..) => true,
    };
    (unchanged(&a) && unchanged(&b)).then(|| (*op, a, b, dest.clone(), *next))
}

/// The last whole assignment to the place `p` (a plain local) among `stmts[..end]`.
fn last_def<'s>(stmts: &'s [Stmt], p: &Place, end: usize) -> Option<(usize, &'s Rvalue)> {
    if !p.proj.is_empty() {
        return None;
    }
    stmts[..end].iter().enumerate().rev().find_map(|(i, s)| match s {
        Stmt::Assign(d, rv) if d.local == p.local => d.proj.is_empty().then_some((i, rv)),
        _ => None,
    })
}

fn assigned(stmts: &[Stmt], l: Local) -> bool {
    stmts
        .iter()
        .any(|s| matches!(s, Stmt::Assign(d, _) if d.local == l))
}

/// The `i32` operand behind the `f64` operand `x` of the statement at `at`: an integral
/// constant, or a local last set (before `at`, in this block) by converting an `i32`.
fn int32_operand(func: &Function, stmts: &[Stmt], x: &Operand, at: usize) -> Option<Operand> {
    match x {
        Operand::Const(c, ty) => int32_const(c, *ty).map(|v| Operand::Const(Const::Int(v), Ty::I32)),
        Operand::Copy(p) => {
            let (j, rv) = last_def(stmts, p, at)?;
            let Rvalue::Cast(src @ Operand::Copy(z), Ty::F64) = rv else {
                return None;
            };
            let z_ok = z.proj.is_empty()
                && func.locals[z.local.0 as usize].ty == Ty::I32
                && !assigned(&stmts[j..at], z.local);
            z_ok.then(|| src.clone())
        }
    }
}

/// `velt_rt_math_add_int32(a, x)` with `x` converted from an `i64` `c` → a branch on
/// |c| <= 2^52 to the integer sum `a + (c as i32)`, keeping the call for other values.
fn converted_sums(externs: &[ExternFn], func: &mut Function) -> bool {
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        let Some(conv) = converted_sum(externs, func, bi) else {
            continue;
        };
        let c = converted_operand(func, conv);
        let Terminator::Call {
            args, dest, next, ..
        } = &func.blocks[bi].term
        else {
            continue;
        };
        let (a, dest, next) = (args[0].clone(), dest.clone(), *next);
        let new_local = |func: &mut Function, ty: Ty| {
            func.locals.push(LocalDecl { ty, name: None });
            Local(func.locals.len() as u32 - 1)
        };
        let (shifted, ok, c32) = (
            new_local(func, Ty::I64),
            new_local(func, Ty::Bool),
            new_local(func, Ty::I32),
        );
        let call = std::mem::replace(&mut func.blocks[bi].term, Terminator::Unreachable);
        let mut fast_stmts = vec![Stmt::Assign(Place::local(c32), Rvalue::Cast(c.clone(), Ty::I32))];
        if let Some(d) = dest {
            fast_stmts.push(Stmt::Assign(
                d,
                Rvalue::Binary(BinOp::Add, a, Operand::Copy(Place::local(c32))),
            ));
        }
        let fast = add_block(func, fast_stmts, Terminator::Goto(next));
        let slow = add_block(func, vec![], call);
        // |c| <= 2^52  ⇔  (c + 2^52) as u64 <= 2^53.
        let shift = Rvalue::Binary(BinOp::Add, c, Operand::Const(Const::Int(1 << 52), Ty::I64));
        push_stmt(func, bi, Stmt::Assign(Place::local(shifted), shift), None);
        let u = new_local(func, Ty::U64);
        push_stmt(
            func,
            bi,
            Stmt::Assign(
                Place::local(u),
                Rvalue::Cast(Operand::Copy(Place::local(shifted)), Ty::U64),
            ),
            None,
        );
        push_stmt(
            func,
            bi,
            Stmt::Assign(
                Place::local(ok),
                Rvalue::Binary(
                    BinOp::Le,
                    Operand::Copy(Place::local(u)),
                    Operand::Const(Const::Int(1 << 53), Ty::U64),
                ),
            ),
            None,
        );
        func.blocks[bi].term = Terminator::Branch {
            cond: Operand::Copy(Place::local(ok)),
            then: fast,
            els: slow,
        };
        changed = true;
    }
    changed
}

/// A new block (without source locations) at the end of `func`.
fn add_block(func: &mut Function, stmts: Vec<Stmt>, term: Terminator) -> velt_vir::vir::BlockId {
    if !func.locs.is_empty() {
        func.locs.push(vec![None; stmts.len() + 1]);
    }
    func.blocks.push(velt_vir::vir::BasicBlock { stmts, term });
    velt_vir::vir::BlockId(func.blocks.len() as u32 - 1)
}

/// The `i64` operand `c` when block `bi` ends in `velt_rt_math_add_int32(a, x)` and `x` was last
/// set in the block by converting `c`, which is unchanged since.
fn converted_sum(externs: &[ExternFn], func: &Function, bi: usize) -> Option<Converted> {
    let block = &func.blocks[bi];
    let Terminator::Call {
        callee: Callee::Extern(id),
        args,
        ..
    } = &block.term
    else {
        return None;
    };
    if externs.get(id.0 as usize)?.symbol != ADD_INT32 {
        return None;
    }
    let [_, Operand::Copy(x)] = args.as_slice() else {
        return None;
    };
    let is_i64 = |p: &Place| p.proj.is_empty() && func.locals[p.local.0 as usize].ty == Ty::I64;
    if let Some((j, rv)) = last_def(&block.stmts, x, block.stmts.len()) {
        let Rvalue::Cast(c @ Operand::Copy(cp), Ty::F64) = rv else {
            return None;
        };
        let ok = is_i64(cp) && !assigned(&block.stmts[j..], cp.local);
        return ok.then(|| Converted::Here(c.clone()));
    }
    // Set once, elsewhere (a loop counter converted at the loop head): the value converted can be
    // kept next to it.
    let usage = Usage::of(func);
    let single = x.proj.is_empty()
        && x.local.0 as usize >= func.params.len()
        && usage.is_register(x.local)
        && usage.get(x.local).defs == 1;
    if !single {
        return None;
    }
    func.blocks.iter().enumerate().find_map(|(b, blk)| {
        blk.stmts.iter().enumerate().find_map(|(j, st)| match st {
            Stmt::Assign(d, Rvalue::Cast(c @ Operand::Copy(cp), Ty::F64))
                if d.local == x.local && d.proj.is_empty() && is_i64(cp) =>
            {
                Some(Converted::At(b, j, c.clone()))
            }
            _ => None,
        })
    })
}

/// Where the `i64` behind a converted operand is found (`converted_sum`).
enum Converted {
    /// This operand, unchanged since the conversion in the same block.
    Here(Operand),
    /// Converted by statement `.1` of block `.0`, the only definition of the converted local.
    At(usize, usize, Operand),
}

/// The `i64` operand behind a `Converted`: for `At`, a new local set right after the conversion
/// (so it always holds the value the converted local was made from).
fn converted_operand(func: &mut Function, conv: Converted) -> Operand {
    match conv {
        Converted::Here(c) => c,
        Converted::At(b, j, c) => {
            func.locals.push(LocalDecl {
                ty: Ty::I64,
                name: None,
            });
            let snap = Local(func.locals.len() as u32 - 1);
            let mut k = 0;
            rewrite_stmts(func, b, |st, out| {
                out.push(st);
                if k == j {
                    out.push(Stmt::Assign(Place::local(snap), Rvalue::Use(c.clone())));
                }
                k += 1;
            });
            Operand::Copy(Place::local(snap))
        }
    }
}

#[cfg(test)]
#[path = "numrep_tests.rs"]
mod tests;
