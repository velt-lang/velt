//! Codegen units: splitting a large program into modules that clang compiles in parallel.
//!
//! clang's time is linear in the size of the IR, so a release build of a large program is one
//! long single-threaded clang run. Large programs (or any, with `VELT_CODEGEN_UNITS`) are split
//! into units of similar size, one module and one object each:
//! - placement (`place`) keeps call chains together: mutually recursive functions and small
//!   callees go to the unit of their first caller, larger callees next to it, and units are
//!   contiguous runs of such groups;
//! - the root module's functions (the entry file's: `own_code`) get a unit of their own, the
//!   last, and the library units are placed from the library's functions alone and import no
//!   body of the root module: an edit of the entry file recompiles that unit only, and the
//!   library units' modules come from the build cache (a static's name comes from its contents,
//!   not its index, so the program's new strings rename nothing in them). An edit that changes
//!   library code still recompiles the library units it changes: a new call of a library
//!   function (velt_opt's inliner shares one budget across the program, so its decisions in
//!   library code depend on the whole program), and a string literal equal to one a library
//!   uses (equal strings are one static, which then becomes shared: its linkage changes in the
//!   library units that define or copy it);
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

pub(crate) use graph::{components, Refs};

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
/// (the largest, `bench/sort.velt`, has about 4 000 statements).
const MIN_UNIT_WEIGHT: usize = 16_000;
/// At most this many units when the count is chosen from the size, up to `MAX_UNIT_WEIGHT`
/// statements per unit. Programs split into 4 run as fast as in one unit (within ±3 %,
/// bench/RESULTS.md); in 8, binary-trees and fib ran 5 % slower (the same machine code, placed
/// differently).
const MAX_UNITS: usize = 4;
/// Larger programs get units of at most this many statements (about 600,000 IR lines; clang takes
/// minutes on one), so that one clang process does not take the whole build: sigx's small-page,
/// 22 million IR lines in 4 units, spent 907 of its 1,091 seconds of code generation in one.
/// Every benchmark program is far below (`MIN_UNIT_WEIGHT`).
const MAX_UNIT_WEIGHT: usize = 200_000;
/// ...at most this many of them.
const MAX_LARGE_UNITS: usize = 16;

/// Number of units for `program`: `requested` (at least 1, at most one per function), else one
/// per `MIN_UNIT_WEIGHT` statements, at most `MAX_UNITS`, one below twice that; a program too
/// large for `MAX_UNITS` units of `MAX_UNIT_WEIGHT` gets units of that weight, at most
/// `MAX_LARGE_UNITS`. The count depends on the program only, never on the machine, so the objects
/// are the same everywhere.
pub(crate) fn unit_count(program: &vir::Program, requested: Option<usize>) -> usize {
    let count = requested.unwrap_or_else(|| count_for(program.funcs.iter().map(weight).sum()));
    count.min(program.funcs.len()).max(1)
}

/// [`unit_count`] of a program of `total` statements.
pub(crate) fn count_for(total: usize) -> usize {
    if total < 2 * MIN_UNIT_WEIGHT {
        1
    } else if total <= MAX_UNITS * MAX_UNIT_WEIGHT {
        (total / MIN_UNIT_WEIGHT).min(MAX_UNITS)
    } else {
        total.div_ceil(MAX_UNIT_WEIGHT).min(MAX_LARGE_UNITS)
    }
}

/// Split `program` into (at most) `count` units of similar weight (see `place`).
pub(crate) fn plan(program: &vir::Program, count: usize) -> Plan {
    let count = count.clamp(1, program.funcs.len().max(1));
    let refs = References::of(program);
    let exported: Vec<bool> = (program.funcs.iter())
        .map(|f| f.linkage == vir::Linkage::Export)
        .collect();
    let owner = place::owners(&exported, &refs.weights, &refs.funcs, &refs.statics, count);
    assemble(program, &refs, &owner)
}

/// [`plan`] for a count chosen from the program's size (`unit_count` without a request): the
/// root module (`own_code`) gets a unit of its own after the `count` library units, which are
/// placed as if it were not there, so an edit of the entry file changes its unit only and the
/// library units' modules, the same as before, come from the build cache. A requested count
/// (`VELT_CODEGEN_UNITS`) is [`plan`]'s: exactly that many units.
pub(crate) fn plan_apart(program: &vir::Program, count: usize) -> Plan {
    let count = count.clamp(1, program.funcs.len().max(1));
    match own_code(program) {
        Some(own) if count >= 2 => {
            let refs = References::of(program);
            let owner = owners_apart(program, &refs, &own, count);
            assemble(program, &refs, &owner)
        }
        _ => plan(program, count),
    }
}

/// Per function: is it the root module's (the program's entry file), not std's, a package's or
/// another module's? Its symbol says (`velt_vir::mangle`): the root module's items are named
/// without a module (`main` → `_V4main`, `helper` → `_V6helper`, `P.get` → `_V1PM3get`), while
/// every other module's are `<module>::<name>` (`_V4utilN3foo`, `_V3stdP2fsN8readFile`). So a
/// symbol whose first segment is followed by its end, a member (`M`), type arguments (`_T`), a
/// suffix (`$poll`) or a closure (`N` and an escaped `{`) is the root module's. Glue (`_G…`)
/// and `velt_main`, which do not change with the program's code, stay with the libraries. The
/// program's other modules are not told apart from packages (both are root-relative paths), so
/// they stay with the libraries too. `None` unless the program has functions of both kinds.
fn own_code(program: &vir::Program) -> Option<Vec<bool>> {
    let own: Vec<bool> = program
        .funcs
        .iter()
        .map(|f| is_root_module(&f.symbol))
        .collect();
    (own.contains(&true) && own.contains(&false)).then_some(own)
}

/// Is `symbol` a function of the root module ([`own_code`])?
pub(crate) fn is_root_module(symbol: &str) -> bool {
    let Some(rest) = symbol.strip_prefix("_V") else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let Ok(len) = rest[..digits].parse::<usize>() else {
        return false;
    };
    let Some(after) = rest.get(digits + len..) else {
        return false;
    };
    after.is_empty()
        || after.starts_with(['M', '$'])
        || after.starts_with("_T")
        || (after.strip_prefix('N')).is_some_and(|n| {
            n.trim_start_matches(|c: char| c.is_ascii_digit())
                .starts_with("_7b")
        })
}

/// [`place::owners`] of the library functions alone, numbered among themselves, as if the
/// program's own functions (`own`) were not there: adding, removing or growing those changes no
/// library unit. The own functions all go to one more unit, the last.
fn owners_apart(
    program: &vir::Program,
    refs: &References,
    own: &[bool],
    count: usize,
) -> Vec<usize> {
    let library: Vec<usize> = (0..own.len()).filter(|&f| !own[f]).collect();
    let mut index = vec![usize::MAX; own.len()];
    for (i, &f) in library.iter().enumerate() {
        index[f] = i;
    }
    let renumber = |r: &Refs| Refs {
        calls: r
            .calls
            .iter()
            .filter(|&&f| !own[f])
            .map(|&f| index[f])
            .collect(),
        addresses: (r.addresses.iter())
            .filter(|&&f| !own[f])
            .map(|&f| index[f])
            .collect(),
        statics: r.statics.clone(),
    };
    let exported: Vec<bool> = (library.iter())
        .map(|&f| program.funcs[f].linkage == vir::Linkage::Export)
        .collect();
    let weights: Vec<usize> = library.iter().map(|&f| refs.weights[f]).collect();
    let funcs: Vec<Refs> = library.iter().map(|&f| renumber(&refs.funcs[f])).collect();
    let statics: Vec<Refs> = refs.statics.iter().map(renumber).collect();
    let placed = place::owners(&exported, &weights, &funcs, &statics, count);
    let units = placed.iter().max().map_or(0, |&u| u + 1);
    let mut owner = vec![units; own.len()];
    for (i, &f) in library.iter().enumerate() {
        owner[f] = placed[i];
    }
    owner
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
    // An imported function (vir.rs invariant 11) is defined by no unit: every unit that refers
    // to it declares it.
    let imported: Vec<bool> = (program.funcs.iter())
        .map(|f| f.linkage == vir::Linkage::Import)
        .collect();
    // A library unit imports no body of the root module's (`own_code`): an edit of the program
    // would change the library unit's module too.
    let own = own_code(program).unwrap_or_default();
    let is_own = |f: usize| own.get(f).copied().unwrap_or(false);
    for (i, &u) in owner.iter().enumerate() {
        if !imported[i] {
            units[u].defines.push(i);
        }
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
                let library_imports_own = is_own(callee) && !is_own(f);
                if owner[callee] != u
                    && weights[callee] <= IMPORT_WEIGHT
                    && !imported[callee]
                    && !library_imports_own
                {
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
            .filter(|f| (owner[*f] != u || imported[*f]) && !imports.contains(f))
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
    own_exported_statics(
        program,
        static_refs,
        owner,
        &imported,
        &mut units[0],
        &mut static_owner,
        &mut static_shared,
        &mut exported,
    );
    let shared = Shared {
        funcs: exported,
        statics: static_shared,
    };
    Plan { units, shared }
}

/// An `Export` static (vir.rs invariant 13) is defined by the program even when none of its
/// bodies reach it: other objects of the link use it (the std cache's served functions, whose
/// bodies are gone). Unit 0 defines each such static and what its relocations reach that no
/// unit defines; it declares the functions they name, which become shared.
#[allow(clippy::too_many_arguments)]
fn own_exported_statics(
    program: &vir::Program,
    static_refs: &[Refs],
    owner: &[usize],
    imported: &[bool],
    unit: &mut Unit,
    static_owner: &mut [Option<usize>],
    static_shared: &mut [bool],
    exported: &mut [bool],
) {
    let mut pending: Vec<usize> = (0..program.statics.len())
        .filter(|&s| {
            program.statics[s].linkage == vir::Linkage::Export && static_owner[s].is_none()
        })
        .collect();
    if pending.is_empty() {
        return;
    }
    let mut placed = BTreeSet::new();
    while let Some(s) = pending.pop() {
        match static_owner[s] {
            None => {
                static_owner[s] = Some(0);
                unit.statics.push(s);
            }
            Some(0) => {}
            Some(_) => {
                if !unit.static_imports.contains(&s) {
                    static_shared[s] = true;
                    unit.static_imports.push(s);
                }
                continue;
            }
        }
        if placed.insert(s) {
            pending.extend(static_refs[s].statics.iter().copied());
        }
    }
    let mut declares: BTreeSet<usize> = unit.declares.iter().copied().collect();
    for &s in &placed {
        for f in static_refs[s].funcs() {
            if owner[f] != 0 || imported[f] {
                exported[f] |= !imported[f];
                if !unit.imports.contains(&f) {
                    declares.insert(f);
                }
            }
        }
    }
    unit.declares = declares.into_iter().collect();
}

/// Size of a function for balancing: statements plus terminators.
fn weight(f: &vir::Function) -> usize {
    f.blocks.iter().map(|b| b.stmts.len() + 1).sum()
}
