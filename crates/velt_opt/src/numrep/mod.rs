//! Integer representation of JS numbers (design #525: step 1's int32 slice, step 2's
//! intraprocedural analysis, step 5's local arrays).
//!
//! A JS `number` is a double, so lowering computes it as `f64`. Where the result is provably
//! identical, this pass stores it as an `i32` or `i64` instead:
//! - **Facts** (`fact`, `flow`, `refine`): an interval, integrality, NaN and `-0` for every
//!   numeric local at every point, refined by branch conditions (and through copies of the
//!   compared values) and widened at loop heads. An array's length is at most 2^53 - 3 (enforced
//!   lower still, `velt_vir::MAX_ARRAY_LEN`), and a
//!   parameter every call sets to a constant starts with that constant's facts (`params`).
//!   A counter (`n++` from a whole constant) is bounded by the trip counts of the loops
//!   around its steps (`counter`).
//! - **Simplification** (`simplify`): comparisons and branches the facts decide become
//!   constants and jumps; `trunc`/`floor`/`ceil`/`round` of whole values, `abs` of
//!   non-negative ones, ToInt32 of constants and int32 values and `floatIndex` of whole
//!   indexes need no call; a parameter known to be one constant is that constant.
//! - **Narrowing** (`narrow`, `rewrite`): an `f64` local whose every value is whole, never NaN
//!   and within ±2^53 (where integer arithmetic is exact), and that is never `-0` or only read
//!   where `-0` cannot be told from 0, becomes an `i32` or `i64` local computed with integer
//!   operations (an `i32` that feeds a 64-bit one becomes 64-bit too). `i64` locals holding only
//!   32-bit values become `i32` (step 1). Other reads convert back exactly.
//! - **Guarded sums** (`int32`): ToInt32 of a sum the facts cannot bound adds as integers
//!   behind a cheap range check.
//! - **Arrays** (`arrays`, step 5): the elements of a `number[]` that never leaves the function
//!   have facts too (a fixpoint over its stores); when they are whole, never NaN or `-0` and
//!   within ±2^53, the elements are stored as `i64`, and narrowing reads them as integers.
//!
//! Without the pass the program computes the same values with doubles; the reference
//! interpreter checks that (`tests.rs`, `tests/random.rs`).

mod arrays;
mod counter;
mod fact;
mod flow;
mod int32;
mod liveness;
mod narrow;
mod params;
mod print;
mod reads;
mod refine;
mod report;
mod rewrite;
mod simplify;

use velt_vir::vir::{
    AggId, AggLayout, Callee, ExternFn, ExternId, Function, Local, Operand, Rvalue, StaticData,
    Stmt, Terminator, Ty,
};

use crate::locals::Usage;
use crate::visit::rvalue_operands;
use flow::Flow;

pub(crate) use print::declare as declare_formatters;
pub use report::Unnarrowed;
pub(crate) use simplify::self_comparisons;

/// The runtime's ToInt32 (`velt_rt_math_to_int32(f64) -> i32`).
const TO_INT32: &str = "velt_rt_math_to_int32";
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
const FLOAT_INDEX: &str = velt_common::std_hook!(float_index);

/// The prelude's int32 helpers (`std/prelude/math.vlt`), inlined in debug builds too so that
/// `numrep` sees their integer paths.
const INT32_HELPERS: [&str; 1] = [velt_common::std_hook!(mul_to_int32)];

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
    /// Per function: is it (an instance of) the prelude's `floatIndex`?
    float_index: Vec<bool>,
    /// The arrays' aggregate: its field 1 is a length, below 2^53.
    array: Option<AggId>,
    /// Per aggregate, its field types: the type of a value read from a field.
    field_tys: std::rc::Rc<[Vec<Ty>]>,
    /// Facts about parameters that every call passes a constant (`params`).
    params: params::ParamFacts,
    /// Number formatters and their integer twins (`print`).
    formatters: Vec<(ExternId, ExternId, bool)>,
}

impl<'p> Env<'p> {
    /// Look up the helpers in `externs` and `funcs`, the arrays' aggregate in `aggs`, and the
    /// facts about parameters from the calls in `funcs` (`statics` hold function addresses).
    pub fn of(
        externs: &'p [ExternFn],
        funcs: &[Function],
        aggs: &[AggLayout],
        statics: &[StaticData],
    ) -> Env<'p> {
        let float_index = instances_of(funcs, &[FLOAT_INDEX]);
        let array = aggs
            .iter()
            .position(|a| a.name == velt_vir::ARRAY_AGG_NAME)
            .map(|i| AggId(i as u32));
        let field_tys = aggs
            .iter()
            .map(|a| a.fields.iter().map(|f| f.0).collect())
            .collect();
        Env {
            externs,
            float_index,
            array,
            field_tys,
            params: params::of(funcs, statics),
            formatters: print::pairs(externs),
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
    let (flow, mut elements) = analyse(env, func);
    if let Some(flow) = flow {
        if simplify::run(env, &flow, func) {
            changed = true;
        }
    }
    if changed {
        crate::simplify_cfg::run(func);
        // Simplifying keeps every value the function stores, so the elements' facts hold
        // while the groups stay the same.
        if !arrays::useful_groups(env, func).same(&elements.groups) {
            elements = analyse(env, func).1;
        }
    }
    changed |= arrays::choose(env, func, &elements, |f| narrow_locals(env, f, &elements));
    changed |= int32::guarded_sums(env, func);
    changed
}

/// Narrow the locals of `func` (the plan and rewrite of `run`), its elements' facts `elements`
/// (computed before `arrays` retyped any); returns whether anything changed.
fn narrow_locals(env: &Env, func: &mut Function, elements: &arrays::Elements) -> bool {
    let tracked = tracked(env, func, &elements.groups);
    if tracked.is_empty() && !constant_to_int32(env, func) {
        return false;
    }
    let Some(flow) = Flow::compute(func, env, &tracked, elements.per_local()) else {
        return false;
    };
    let plan = narrow::plan(env, &flow, func);
    rewrite::apply(env, func, &plan)
}

/// The `f64` locals of `func` inside loops that stay `f64`, each with the reason
/// (`velt build --report numbers`).
pub(crate) fn unnarrowed(env: &Env, func: &Function) -> Vec<Unnarrowed> {
    match analyse(env, func).0 {
        Some(flow) => report::unnarrowed(env, &flow, func),
        None => vec![],
    }
}

/// Facts for the locals of `func` that matter (`None`: nothing to narrow, or too big), and
/// for the elements of its arrays.
fn analyse(env: &Env, func: &Function) -> (Option<Flow>, arrays::Elements) {
    let groups = arrays::useful_groups(env, func);
    let tracked = tracked(env, func, &groups);
    if tracked.is_empty() && !constant_to_int32(env, func) {
        return (None, arrays::Elements::unknown(groups));
    }
    let (elements, flow) = arrays::Elements::of(env, func, &tracked, groups);
    let flow = flow.or_else(|| Flow::compute(func, env, &tracked, elements.per_local()));
    (flow, elements)
}

/// Does `func` take ToInt32 of a constant (`simplify` folds it without tracking anything)?
fn constant_to_int32(env: &Env, func: &Function) -> bool {
    func.blocks.iter().any(|b| {
        matches!(&b.term, Terminator::Call { callee, args, .. }
            if env.is_to_int32(callee) && matches!(args.as_slice(), [Operand::Const(..)]))
    })
}

fn numeric(ty: Ty) -> bool {
    ty.is_int() || ty.is_float() || ty == Ty::Bool
}

/// Register-like `f64` locals, `i64` locals set from 32-bit values, the arguments of the calls
/// `simplify` folds, the values stored into and loaded from the elements of `groups`
/// (`arrays`), and everything flowing into them or compared with them; then the booleans
/// comparing any of these.
fn tracked(env: &Env, func: &Function, groups: &arrays::Groups) -> Vec<Local> {
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
            if groups.of_element(d).is_some() {
                seeds.extend(locals);
            }
            continue;
        }
        if let Rvalue::Use(Operand::Copy(p)) = rv {
            if groups.of_element(p).is_some() {
                seeds.push(d.local);
            }
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
    let mut counters_seen = false;
    let counter_cx = counter::Cx::new(func);
    loop {
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
        // A counter's cap needs the trip counts of the loops around it: track what their exit
        // tests compare too (`i < s.length` with an `i64` `i`). Also for an integer counter
        // read as a double (`page[q++]` once `q` is an `i64`: `q as f64` is whole only within
        // the cap).
        let counter = |i: usize| {
            let t = ty(Local(i as u32));
            in_slice[i]
                && (t == Ty::F64 || t.is_int())
                && counter::maybe(&counter_cx, Local(i as u32), true)
        };
        if counters_seen || !(0..n).any(counter) {
            break;
        }
        counters_seen = true;
        for &(_, a, b) in &compared {
            work.extend([a, b].into_iter().filter(|&x| reg(x)));
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
    let env = Env::of(
        &program.externs,
        &program.funcs,
        &program.aggs,
        &program.statics,
    );
    let mut changed = false;
    for f in &mut program.funcs {
        changed |= run(&env, f);
    }
    changed
}

#[cfg(test)]
mod arrays_tests;
#[cfg(test)]
mod tests;
