//! Statics with identity in served code: named, so a cached object uses the program's copy.
//!
//! A vtable, a type descriptor, an `any` table or a module's state is compared or written by
//! address, so a cached function's object can't carry a copy of its own. Instead the static gets
//! a symbol (vir invariant 13) made from its contents, the same in every program that has the
//! same static: the program defines it (`Export`), and the function's program of its own
//! declares it (`Import`, with its contents for reading, so the code is optimized as the program
//! would optimize it). What such a static's relocations point at is named too, and the functions
//! they name are exported, since the object's code may load them from the static and call them.
//!
//! A name has to be unique in the program: two statics with the same contents (which lowering
//! rarely makes) would get one, so neither is named, and code reaching them stays in the program.

use std::collections::{HashMap, HashSet};

use velt_vir::fingerprint::Canon;
use velt_vir::vir::{self, Const, Linkage, Operand};

use super::select;

/// The symbol each nameable static of a program would get.
pub(super) struct Names {
    names: Vec<Option<String>>,
}

impl Names {
    /// Names for `program`'s statics, from their contents (`canon`, `Mode::Link`).
    pub fn new(program: &vir::Program, canon: &Canon) -> Names {
        let prints: Vec<_> = (0..program.statics.len())
            .map(|i| canon.static_fingerprint(vir::StaticId(i as u32)))
            .collect();
        let mut count: HashMap<_, usize> = HashMap::new();
        for p in &prints {
            *count.entry(*p).or_default() += 1;
        }
        let names = (prints.iter())
            .map(|p| (count[p] == 1).then(|| format!("_Vs{p:032x}")))
            .collect();
        Names { names }
    }

    /// The symbol of static `s`, `None` when another static has the same contents.
    pub fn of(&self, s: usize) -> Option<&str> {
        self.names.get(s).and_then(|n| n.as_deref())
    }
}

/// Whether static `s` has identity: relocations (its address is in other data, or other
/// addresses are in it), or written.
fn has_identity(program: &vir::Program, s: usize) -> bool {
    let data = &program.statics[s];
    !data.relocs.is_empty() || data.mutable.is_some()
}

/// The statics the code of `set` (an `inlinable` set) reaches that must be the program's own:
/// each one with identity it names, and everything their relocations reach. In a deterministic
/// order.
pub(super) fn reached(program: &vir::Program, set: &[usize]) -> Vec<usize> {
    let mut seen = HashSet::new();
    let mut order = vec![];
    for &j in set {
        select::visit(&program.funcs[j], &mut |op| {
            if let Operand::Const(Const::Static(s), _) = op {
                let s = s.0 as usize;
                if has_identity(program, s) && seen.insert(s) {
                    order.push(s);
                }
            }
        });
    }
    let mut n = 0;
    while let Some(&s) = order.get(n) {
        n += 1;
        for (_, target) in &program.statics[s].relocs {
            if let Const::Static(t) = target {
                if seen.insert(t.0 as usize) {
                    order.push(t.0 as usize);
                }
            }
        }
    }
    order
}

/// The functions the relocations of `statics` name, in a deterministic order.
pub(super) fn functions(program: &vir::Program, statics: &[usize]) -> Vec<usize> {
    let mut seen = HashSet::new();
    let mut out = vec![];
    for &s in statics {
        for (_, target) in &program.statics[s].relocs {
            if let Const::Func(f) = target {
                if seen.insert(f.0) {
                    out.push(f.0 as usize);
                }
            }
        }
    }
    out
}

/// Whether every static of `statics` can be named.
pub(super) fn nameable(names: &Names, statics: &[usize]) -> bool {
    statics.iter().all(|&s| names.of(s).is_some())
}

/// Name `statics` in `program` and make them, and the functions their relocations name,
/// visible to the link's other objects.
pub(super) fn export(program: &mut vir::Program, names: &Names, statics: &[usize]) {
    for f in functions(program, statics) {
        let f = &mut program.funcs[f];
        if f.linkage == Linkage::Internal {
            f.linkage = Linkage::Export;
        }
    }
    for &s in statics {
        let data = &mut program.statics[s];
        if data.symbol.is_none() {
            data.symbol = names.of(s).map(str::to_string);
            data.linkage = Linkage::Export;
        }
    }
}
