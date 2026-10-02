//! Codegen units: splitting a large program into modules that clang compiles in parallel.
//!
//! clang's time is linear in the size of the IR, so a release build of a large program is one
//! long single-threaded clang run. Large programs (or any, with `VELT_CODEGEN_UNITS`) are split
//! into units of similar size, one module and one object each:
//! - placement (`place`) keeps call chains together: mutually recursive functions and small
//!   callees go to the unit of their first caller, larger callees next to it, and units are
//!   contiguous runs of such groups;
//! - an internal function referenced from another unit (call, address, vtable slot) becomes a
//!   `hidden` external symbol of its unit (its name is already unique in the program), and the
//!   other units declare it `hidden` too, so their calls and address computations are direct and
//!   PC-relative (no PLT or GOT); an exported function (`velt_main`) keeps default visibility,
//!   since the shared runtime of debug builds finds it by name, and is declared `dso_local`;
//! - a static is defined by the first unit that uses it (`hidden` when others use it too: the
//!   runtime compares some static addresses, e.g. type descriptors, so there must be one copy);
//!   the other units get an `available_externally` copy, so its contents stay visible to their
//!   optimizer (devirtualization through vtables);
//! - small functions of other units that a unit's (not huge) functions call are imported as
//!   `available_externally` copies (like ThinLTO's function import): LLVM can inline them, and
//!   emits no code for them.
//!
//! The rest of cross-unit optimization is lost (inlining of larger callees and interprocedural
//! analyses of the functions made external). With placement keeping hot call chains together,
//! split programs run as fast as unsplit ones, but small programs are one unit (`unit_count`).
//!
//! Modules: `graph` (references, the reference graph and its components), `place` (which unit
//! defines each function).

mod graph;
mod place;

use std::collections::BTreeSet;

use velt_vir::vir;

use graph::Refs;

/// Functions of at most this weight are imported into the units that call them...
const IMPORT_WEIGHT: usize = 40;
/// ...from callers of at most this weight. Inlining thousands of callees into one huge function
/// (a generated `main`) made its unit slower than the whole program in one unit (long_main_4000:
/// 218 s against 138 s; without imports 106 s), and buys little at run time.
const IMPORT_CALLER_WEIGHT: usize = 5_000;

/// How one unit's module is made.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Unit {
    /// Functions defined by this unit (indices into `program.funcs`, ascending).
    pub defines: Vec<usize>,
    /// Functions of other units copied in as `available_externally` (ascending).
    pub imports: Vec<usize>,
    /// Functions of other units referenced but not imported (declared; ascending).
    pub declares: Vec<usize>,
    /// Statics the unit defines: the ones its functions use, directly or through other statics,
    /// that no earlier unit uses (ascending).
    pub statics: Vec<usize>,
    /// Statics an earlier unit defines that this one uses (`available_externally`; ascending).
    pub static_imports: Vec<usize>,
}

/// Which definitions other units refer to: they get `hidden` instead of local linkage.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Shared {
    /// Per function: referenced from a unit that does not define it.
    pub funcs: Vec<bool>,
    /// Per static: used by a unit that does not define it.
    pub statics: Vec<bool>,
}

/// A split of the program into units.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub units: Vec<Unit>,
    pub shared: Shared,
}

/// Statements (VIR, after `velt_opt`) per unit when the count is chosen from the program's size:
/// a program below twice this is one unit. Small programs build fast in one unit and lose the
/// most to a split (cross-unit calls in their hot loops); every benchmark program stays one unit
/// (the largest, `bench/sort.vlt`, has about 4 000 statements).
const MIN_UNIT_WEIGHT: usize = 16_000;
/// At most this many units when the count is chosen from the size. Programs split into 4 run as
/// fast as in one unit (within ±3 %, bench/RESULTS.md); in 8, binary-trees and fib ran 5 % slower
/// (the same machine code, placed differently).
const MAX_UNITS: usize = 4;

/// Number of units for `program`: `requested` (at least 1, at most one per function), else one
/// per `MIN_UNIT_WEIGHT` statements, at most `MAX_UNITS`, one below twice that. The count depends
/// on the program only, never on the machine, so the objects are the same everywhere.
pub(crate) fn unit_count(program: &vir::Program, requested: Option<usize>) -> usize {
    let count = requested.unwrap_or_else(|| {
        let total: usize = program.funcs.iter().map(weight).sum();
        if total < 2 * MIN_UNIT_WEIGHT {
            1
        } else {
            (total / MIN_UNIT_WEIGHT).min(MAX_UNITS)
        }
    });
    count.min(program.funcs.len()).max(1)
}

/// Split `program` into (at most) `count` units of similar weight (see `place`).
pub(crate) fn plan(program: &vir::Program, count: usize) -> Plan {
    let count = count.clamp(1, program.funcs.len().max(1));
    let refs = References::of(program);
    let owner = place::owners(program, &refs.weights, &refs.funcs, &refs.statics, count);
    assemble(program, &refs, &owner)
}

/// The plan for a given unit of every function (`owner`; units numbered from 0, none empty).
#[cfg(test)]
pub(crate) fn plan_placed(program: &vir::Program, owner: &[usize]) -> Plan {
    assemble(program, &References::of(program), owner)
}

/// Weights and references of a program's functions and statics.
struct References {
    weights: Vec<usize>,
    funcs: Vec<Refs>,
    statics: Vec<Refs>,
}

impl References {
    fn of(program: &vir::Program) -> References {
        let (n, n_statics) = (program.funcs.len(), program.statics.len());
        // `velt_vir::verify` guarantees valid ids; out-of-range ones are dropped all the same.
        References {
            weights: program.funcs.iter().map(weight).collect(),
            funcs: (program.funcs.iter())
                .map(|f| Refs::of_function(f).within(n, n_statics))
                .collect(),
            statics: (program.statics.iter())
                .map(|s| Refs::of_static(s).within(n, n_statics))
                .collect(),
        }
    }
}

/// Imports, declarations and statics of every unit, given the unit of every function.
fn assemble(program: &vir::Program, refs: &References, owner: &[usize]) -> Plan {
    let (n, n_statics) = (program.funcs.len(), program.statics.len());
    let (weights, static_refs, refs) = (&refs.weights, &refs.statics, &refs.funcs);
    let used = owner.iter().max().map_or(1, |&u| u + 1);
    let mut units: Vec<Unit> = (0..used).map(|_| Unit::default()).collect();
    for (i, &u) in owner.iter().enumerate() {
        units[u].defines.push(i);
    }
    let mut exported = vec![false; n];
    let mut static_owner: Vec<Option<usize>> = vec![None; n_statics];
    let mut static_shared = vec![false; n_statics];
    for (u, unit) in units.iter_mut().enumerate() {
        // Functions whose bodies this unit contains: its own, then the imported ones.
        let mut bodies: BTreeSet<usize> = unit.defines.iter().copied().collect();
        let mut imports = BTreeSet::new();
        for &f in unit
            .defines
            .iter()
            .filter(|&&f| weights[f] <= IMPORT_CALLER_WEIGHT)
        {
            for &callee in &refs[f].calls {
                if owner[callee] != u && weights[callee] <= IMPORT_WEIGHT {
                    imports.insert(callee);
                }
            }
        }
        bodies.extend(&imports);
        let mut statics = BTreeSet::new();
        let mut pending: Vec<usize> = bodies
            .iter()
            .flat_map(|&f| refs[f].statics.iter().copied())
            .collect();
        while let Some(s) = pending.pop() {
            if statics.insert(s) {
                pending.extend(static_refs[s].statics.iter().copied());
            }
        }
        let referenced: BTreeSet<usize> = bodies
            .iter()
            .flat_map(|&f| refs[f].funcs())
            .chain(statics.iter().flat_map(|&s| static_refs[s].funcs()))
            .collect();
        for &f in &referenced {
            if owner[f] != u {
                exported[f] = true;
            }
        }
        unit.declares = referenced
            .into_iter()
            .filter(|f| owner[*f] != u && !imports.contains(f))
            .collect();
        unit.imports = imports.into_iter().collect();
        for s in statics {
            match static_owner[s] {
                None => {
                    static_owner[s] = Some(u);
                    unit.statics.push(s);
                }
                Some(_) => {
                    static_shared[s] = true;
                    unit.static_imports.push(s);
                }
            }
        }
    }
    let shared = Shared {
        funcs: exported,
        statics: static_shared,
    };
    Plan { units, shared }
}

/// Size of a function for balancing: statements plus terminators.
fn weight(f: &vir::Function) -> usize {
    f.blocks.iter().map(|b| b.stmts.len() + 1).sum()
}
