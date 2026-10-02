//! Codegen units: splitting a large program into modules that clang compiles in parallel.
//!
//! clang's time is linear in the size of the IR, so a release build of a large program is one
//! long single-threaded clang run. When asked for (`VELT_CODEGEN_UNITS`), the functions are split,
//! in program order, into contiguous runs of similar size, one module and one object each:
//! - an internal function referenced from another unit (call, address, vtable slot) becomes a
//!   `hidden` external symbol of its unit (its name is already unique in the program);
//! - a static is defined by the first unit that uses it (`hidden` when others use it too: the
//!   runtime compares some static addresses, e.g. type descriptors, so there must be one copy);
//!   the other units get an `available_externally` copy, so its contents stay visible to their
//!   optimizer (devirtualization through vtables);
//! - small functions of other units that a unit's (not huge) functions call are imported as
//!   `available_externally` copies (like ThinLTO's function import): LLVM can inline them, and
//!   emits no code for them.
//!
//! The rest of cross-unit optimization is lost (inlining of larger callees, interprocedural
//! analyses of the functions made external, and inlining through recursive calls that cross
//! units, which imports don't restore: freeing a binary tree runs 13 % slower in four units), so
//! programs are one unit unless more are requested.

use std::collections::BTreeSet;

use velt_vir::vir::{self, Callee, Const, Operand, Rvalue, Stmt, Terminator};

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

/// Number of units for `program`: `requested` (at least 1, at most one per function), else 1.
pub(crate) fn unit_count(program: &vir::Program, requested: Option<usize>) -> usize {
    requested.unwrap_or(1).min(program.funcs.len()).max(1)
}

/// Split `program` into (at most) `count` units of similar weight.
pub(crate) fn plan(program: &vir::Program, count: usize) -> Plan {
    let n = program.funcs.len();
    let weights: Vec<usize> = program.funcs.iter().map(weight).collect();
    let total: usize = weights.iter().sum();
    let count = count.clamp(1, n.max(1));

    // Contiguous runs: unit k ends once the running total reaches (k + 1) / count of the total.
    let mut owner = vec![0usize; n];
    let mut acc = 0usize;
    let mut unit = 0usize;
    for (i, w) in weights.iter().enumerate() {
        owner[i] = unit;
        acc += w;
        if unit + 1 < count && acc * count >= total * (unit + 1) {
            unit += 1;
        }
    }
    let used = owner.last().map_or(1, |&u| u + 1);

    // `velt_vir::verify` guarantees valid ids; out-of-range ones are dropped all the same.
    let (n_statics, valid) = (program.statics.len(), |r: Refs| {
        r.within(n, program.statics.len())
    });
    let refs: Vec<Refs> = program
        .funcs
        .iter()
        .map(|f| valid(Refs::of_function(f)))
        .collect();
    let static_refs: Vec<Refs> = program
        .statics
        .iter()
        .map(|s| valid(Refs::of_static(s)))
        .collect();
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

/// Program entities a function or static refers to.
#[derive(Default)]
struct Refs {
    /// Functions called directly.
    calls: BTreeSet<usize>,
    /// Functions whose address is taken.
    addresses: BTreeSet<usize>,
    statics: BTreeSet<usize>,
}

impl Refs {
    /// Without references to functions ≥ `funcs` or statics ≥ `statics`.
    fn within(mut self, funcs: usize, statics: usize) -> Refs {
        self.calls.retain(|&f| f < funcs);
        self.addresses.retain(|&f| f < funcs);
        self.statics.retain(|&s| s < statics);
        self
    }

    fn funcs(&self) -> impl Iterator<Item = usize> + '_ {
        self.calls.iter().chain(&self.addresses).copied()
    }

    fn of_function(f: &vir::Function) -> Refs {
        let mut r = Refs::default();
        for b in &f.blocks {
            for s in &b.stmts {
                match s {
                    Stmt::Assign(_, rv) => match rv {
                        Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => r.operand(a),
                        Rvalue::Binary(_, a, b) => {
                            r.operand(a);
                            r.operand(b);
                        }
                        Rvalue::Aggregate(_, ops) => ops.iter().for_each(|o| r.operand(o)),
                        Rvalue::AddrOf(_) => {}
                    },
                    Stmt::MemCopy { dst, src, .. } => {
                        r.operand(dst);
                        r.operand(src);
                    }
                    Stmt::MemCopyDyn { dst, src, len, .. } => {
                        r.operand(dst);
                        r.operand(src);
                        r.operand(len);
                    }
                    Stmt::MemSet { dst, byte, len } => {
                        r.operand(dst);
                        r.operand(byte);
                        r.operand(len);
                    }
                    Stmt::Nop => {}
                }
            }
            match &b.term {
                Terminator::Branch { cond: o, .. }
                | Terminator::Switch { value: o, .. }
                | Terminator::Return(o) => r.operand(o),
                Terminator::Call { callee, args, .. } => {
                    match callee {
                        Callee::Func(id) => {
                            r.calls.insert(id.0 as usize);
                        }
                        Callee::Extern(_) => {}
                        Callee::Ptr { target, .. } => r.operand(target),
                    }
                    args.iter().for_each(|o| r.operand(o));
                }
                Terminator::Goto(_) | Terminator::Unreachable => {}
            }
        }
        r
    }

    fn of_static(s: &vir::StaticData) -> Refs {
        let mut r = Refs::default();
        for (_, target) in &s.relocs {
            r.constant(target);
        }
        r
    }

    fn operand(&mut self, o: &Operand) {
        if let Operand::Const(c, _) = o {
            self.constant(c);
        }
    }

    fn constant(&mut self, c: &Const) {
        match c {
            Const::Func(id) => {
                self.addresses.insert(id.0 as usize);
            }
            Const::Static(id) => {
                self.statics.insert(id.0 as usize);
            }
            _ => {}
        }
    }
}
