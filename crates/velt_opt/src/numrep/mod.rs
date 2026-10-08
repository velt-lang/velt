//! Integer representation of JS numbers (design #525: step 1's int32 slice, step 2's
//! intraprocedural analysis).
//!
//! A JS `number` is a double, so lowering computes it as `f64`. Where the result is provably
//! identical, this pass stores it as an `i32` or `i64` instead:
//! - **Facts** (`fact`, `flow`, `refine`): an interval, integrality, NaN and `-0` for every
//!   numeric local at every point, refined by branch conditions and widened at loop heads.
//! - **Simplification** (`simplify`): comparisons and branches the facts decide become
//!   constants and jumps; `trunc`/`floor`/`ceil`/`round` of whole values, `abs` of
//!   non-negative ones and `__floatIndex` of whole indexes need no call.
//! - **Narrowing** (`narrow`, `rewrite`): an `f64` local whose every value is whole, never NaN
//!   and within ±2^53 (where integer arithmetic is exact), and that is never `-0` or only read
//!   where `-0` cannot be told from 0, becomes an `i32` or `i64` local computed with integer
//!   operations. `i64` locals holding only 32-bit values become `i32` (step 1). Other reads
//!   convert back exactly.
//! - **Guarded sums** (`int32`): ToInt32 of a sum the facts cannot bound adds as integers
//!   behind a cheap range check.
//!
//! Without the pass the program computes the same values with doubles; the reference
//! interpreter checks that (`tests.rs`, `tests/random.rs`).

mod fact;
mod flow;
mod int32;
mod narrow;
mod reads;
mod refine;
mod report;
mod rewrite;
mod simplify;

use velt_vir::vir::{
    Callee, ExternFn, ExternId, Function, Local, Operand, Rvalue, Stmt, Terminator, Ty,
};

use crate::locals::Usage;
use crate::visit::rvalue_operands;
use flow::Flow;

pub use report::Unnarrowed;
pub(crate) use simplify::self_comparisons;

/// The runtime's ToInt32 (`velt_rt_math_to_int32(f64) -> i32`).
const TO_INT32: &str = "velt_rt_math_to_int32";
/// ToInt32 of `a + x` for an int32 `a` (`velt_rt_math_add_int32(i32, f64) -> i32`).
const ADD_INT32: &str = "velt_rt_math_add_int32";
/// `Math.trunc`, `floor`, `ceil`, `round` (`f64 -> f64`).
const ROUNDING: [&str; 4] = [
    "velt_rt_math_trunc",
    "velt_rt_math_floor",
    "velt_rt_math_ceil",
    "velt_rt_math_round",
];
/// `Math.abs` (`f64 -> f64`).
const ABS: &str = "velt_rt_math_fabs";
/// The prelude's `xs[i]` with a float `i` (`std/prelude/math.vlt`).
const FLOAT_INDEX: &str = "std/prelude/math::__floatIndex";

/// The prelude's int32 helpers (`std/prelude/math.vlt`), inlined in debug builds too so that
/// `numrep` sees their integer paths.
const INT32_HELPERS: [&str; 2] = [
    "std/prelude/math::__mulToInt32",
    "std/prelude/math::__intAddToInt32",
];

/// Per function: is it (an instance of) one of the prelude functions `names`? Instances made
/// for a caller's location (panic messages) append `_L…` to the symbol.
fn instances_of(funcs: &[Function], names: &[&str]) -> Vec<bool> {
    let bases: Vec<String> = names.iter().map(|n| velt_vir::mangle::mangle(n)).collect();
    funcs
        .iter()
        .map(|f| {
            bases.iter().any(|base| {
                f.symbol == *base
                    || f.symbol
                        .strip_prefix(base.as_str())
                        .is_some_and(|rest| rest.starts_with("_L"))
            })
        })
        .collect()
}

/// Per function: is it one of the int32 helpers that debug builds inline?
pub(crate) fn int32_helpers(funcs: &[Function]) -> Vec<bool> {
    instances_of(funcs, &INT32_HELPERS)
}

/// The program-wide names `numrep` recognizes.
pub(crate) struct Env<'p> {
    externs: &'p [ExternFn],
    /// Per function: is it (an instance of) the prelude's `__floatIndex`?
    float_index: Vec<bool>,
}

impl<'p> Env<'p> {
    /// Look up the helpers in `externs` and `funcs`.
    pub fn of(externs: &'p [ExternFn], funcs: &[Function]) -> Env<'p> {
        let float_index = instances_of(funcs, &[FLOAT_INDEX]);
        Env {
            externs,
            float_index,
        }
    }

    fn symbol(&self, id: ExternId) -> &str {
        self.externs
            .get(id.0 as usize)
            .map_or("", |e| e.symbol.as_str())
    }

    fn is_rounding(&self, id: ExternId) -> bool {
        ROUNDING.contains(&self.symbol(id))
    }

    fn is_abs(&self, id: ExternId) -> bool {
        self.symbol(id) == ABS
    }

    fn is_to_int32(&self, callee: &Callee) -> bool {
        matches!(callee, Callee::Extern(id) if self.symbol(*id) == TO_INT32)
    }

    fn is_float_index(&self, callee: &Callee) -> bool {
        matches!(callee, Callee::Func(f) if self.float_index.get(f.0 as usize) == Some(&true))
    }
}

/// Narrow the number locals of `func`; returns whether anything changed.
pub(crate) fn run(env: &Env, func: &mut Function) -> bool {
    let mut changed = false;
    if let Some(flow) = analyse(env, func) {
        if simplify::run(env, &flow, func) {
            changed = true;
        }
    }
    if changed {
        crate::simplify_cfg::run(func);
    }
    if let Some(flow) = analyse(env, func) {
        let plan = narrow::plan(env, &flow, func);
        changed |= rewrite::apply(env, func, &plan);
    }
    changed |= int32::guarded_sums(env, func);
    changed
}

/// The `f64` locals of `func` inside loops that stay `f64`, each with the reason
/// (`velt build --report numbers`).
pub(crate) fn unnarrowed(env: &Env, func: &Function) -> Vec<Unnarrowed> {
    match analyse(env, func) {
        Some(flow) => report::unnarrowed(env, &flow, func),
        None => vec![],
    }
}

/// Facts for the locals of `func` that matter (`None`: nothing to narrow, or too big).
fn analyse(env: &Env, func: &Function) -> Option<Flow> {
    let tracked = tracked(env, func);
    if tracked.is_empty() {
        return None;
    }
    Flow::compute(func, env, &tracked)
}

fn numeric(ty: Ty) -> bool {
    ty.is_int() || ty.is_float() || ty == Ty::Bool
}

/// Register-like `f64` locals, `i64` locals set from 32-bit values, the arguments of the calls
/// `simplify` folds, and everything flowing into them or compared with them; then the
/// booleans comparing any of these.
fn tracked(env: &Env, func: &Function) -> Vec<Local> {
    let usage = Usage::of(func);
    let n = func.locals.len();
    let reg = |l: Local| usage.is_register(l) && numeric(func.locals[l.0 as usize].ty);
    let ty = |l: Local| func.locals[l.0 as usize].ty;
    let mut inputs: Vec<Vec<Local>> = vec![vec![]; n];
    let mut compared: Vec<(Local, Local, Local)> = vec![];
    let mut seeds: Vec<Local> = vec![];
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        let Stmt::Assign(d, rv) = s else { continue };
        let mut locals = vec![];
        rvalue_operands(rv, &mut |op| {
            if let Operand::Copy(p) = op {
                if p.proj.is_empty() {
                    locals.push(p.local);
                }
            }
        });
        if !d.proj.is_empty() {
            continue;
        }
        if let Rvalue::Binary(op, ..) = rv {
            if flow::is_comparison(*op) {
                match locals[..] {
                    [a, b] => compared.push((d.local, a, b)),
                    [a] => compared.push((d.local, a, a)),
                    _ => {}
                }
                continue;
            }
        }
        let l = d.local;
        if ty(l) == Ty::F64 || (ty(l) == Ty::I64 && int32::from_small_int(func, rv)) {
            seeds.push(l);
        }
        inputs[l.0 as usize].extend(locals);
    }
    for b in &func.blocks {
        if let Terminator::Call { dest: Some(d), .. } = &b.term {
            if d.proj.is_empty() && ty(d.local) == Ty::F64 {
                seeds.push(d.local);
            }
        }
        if let Terminator::Call { callee, args, .. } = &b.term {
            let folded = match callee {
                Callee::Extern(id) => env.is_rounding(*id) || env.is_abs(*id),
                c => env.is_to_int32(c) || env.is_float_index(c),
            };
            if folded {
                for a in args {
                    if let Operand::Copy(p) = a {
                        seeds.push(p.local);
                    }
                }
            }
        }
    }
    let mut in_slice = vec![false; n];
    let mut work: Vec<Local> = seeds.into_iter().filter(|&l| reg(l)).collect();
    while let Some(l) = work.pop() {
        if std::mem::replace(&mut in_slice[l.0 as usize], true) {
            continue;
        }
        work.extend(inputs[l.0 as usize].iter().copied().filter(|&x| reg(x)));
        for &(_, a, b) in &compared {
            let other = if a == l {
                b
            } else if b == l {
                a
            } else {
                continue;
            };
            if reg(other) {
                work.push(other);
            }
        }
    }
    // Booleans from comparisons of tracked values, so branches on them can be decided.
    for &(c, a, b) in &compared {
        if (in_slice[a.0 as usize] || in_slice[b.0 as usize]) && reg(c) {
            in_slice[c.0 as usize] = true;
        }
    }
    (0..n as u32)
        .map(Local)
        .filter(|l| in_slice[l.0 as usize])
        .collect()
}

/// Narrowing for `program`'s functions, for tests of the whole pass.
#[cfg(test)]
pub(crate) fn run_program(program: &mut velt_vir::vir::Program) -> bool {
    let env = Env::of(&program.externs, &program.funcs);
    let mut changed = false;
    for f in &mut program.funcs {
        changed |= run(&env, f);
    }
    changed
}

#[cfg(test)]
mod tests;
