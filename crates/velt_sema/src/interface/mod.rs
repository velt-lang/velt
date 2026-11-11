//! Interface summaries (docs/internals/design/dev-warm-front-end.md step 3, "B. Interface
//! summaries"): what other code observes of each function of a checked program, as a hash per
//! function, and the program-wide facts a body can change, as a hash per kind of fact. Two
//! versions of a program whose summaries differ only in the functions a save edited (and their
//! closures) have the same interfaces: nothing that depends on those functions needs to be
//! checked or lowered again (early cutoff).
//!
//! A function's interface is its signature as sema finished it: type parameters, parameter types
//! and pass modes (ownership), which parameters it modifies (write effects), the result type
//! (inferred or written), the error set, async and generator, `this`, its captures' modes, and
//! what the program-wide tables say of it (a fresh closure, a lock-free callback, a CPU target,
//! its value's `name` and `length`). Program-wide facts are every other field of the program
//! (`tables.rs`): the declarations other than functions (with the flags sema infers on them from
//! every body, such as a field being assigned somewhere), the implementations and conversions,
//! the type tables, the counted object types, the module state, the entry, the std definitions
//! the program reaches, the generated instances, what the dynamic tables hold, the decorators and
//! C exports, and the program's flags. The HIR structs are destructured without `..`, so a new
//! field fails to build until it is hashed here or skipped with a reason.
//!
//! Everything is hashed by structure and by name, never by `TyId`, `DefId` or `Span`: those
//! differ between two checks of programs that differ in one body (types are interned and
//! definitions added in checking order, and an edit moves the code after it). A closure is named
//! after the function it is in and its number there (`f::{closure#0}`), so its name survives an
//! edit of another function. Not covered, by design: initializer expressions of fields and
//! globals (declarations: change detection classifies their edits as other changes), and facts
//! sema keeps to itself without putting them in the HIR (which functions return fresh values,
//! `fresh_returns`), which the per-function passes read from the kept checking context.

mod hashing;
mod tables;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::hir::{self, Def, DefId};
use hashing::Hashing;

/// The interfaces of a checked program's functions and its program-wide facts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Per function, by its qualified name (`main`, `User.greet`, `std/fs::readFile`,
    /// `main::{closure#0}`): names are unique, but one built from display text could repeat, so
    /// each name keeps the sorted hashes of every function that has it.
    fns: FxHashMap<String, Vec<u64>>,
    /// Per kind of fact, in a fixed order.
    facts: Vec<(&'static str, u64)>,
    /// The std functions lazy std left unchecked (stubs): a body edit that makes the program
    /// reach one is a change of the `reached std` fact, not of the function's interface.
    unchecked: FxHashSet<String>,
}

/// One difference between two summaries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// A function whose interface changed, or that one version has and the other doesn't.
    Fn(String),
    /// A kind of program-wide fact (`declarations`, `reached std`, ...).
    Fact(&'static str),
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Change::Fn(name) => write!(f, "{name}"),
            Change::Fact(what) => write!(f, "the program's {what}"),
        }
    }
}

impl Summary {
    /// The summary of a checked program.
    pub fn of(p: &hir::Program) -> Summary {
        let mut h = Hashing::new(p);
        let mut fns: FxHashMap<String, Vec<u64>> = FxHashMap::default();
        let mut unchecked = FxHashSet::default();
        for (i, d) in p.defs.iter().enumerate() {
            let Def::Fn(f) = d else { continue };
            let id = DefId(i as u32);
            if p.unchecked.contains(&id) {
                unchecked.insert(f.name.clone());
            } else {
                let hash = h.function(id, f);
                fns.entry(f.name.clone()).or_default().push(hash);
            }
        }
        fns.values_mut().for_each(|hashes| hashes.sort_unstable());
        let facts = tables::facts(&mut h);
        Summary {
            fns,
            facts,
            unchecked,
        }
    }

    /// How `newer` differs from `self`: facts in their fixed order, then functions by name. A
    /// std function one version checked and the other left unchecked, or one only one version
    /// has (its module loaded on demand), is part of the `reached std` fact.
    pub fn changes(&self, newer: &Summary) -> Vec<Change> {
        let mut out: Vec<Change> = (self.facts.iter().zip(&newer.facts))
            .filter(|(a, b)| a != b)
            .map(|(a, _)| Change::Fact(a.0))
            .collect();
        // A closure of an unchecked function has no definition in that version.
        let outer = |name: &str| -> String {
            let at = name.find("::{closure#").unwrap_or(name.len());
            name[..at].to_string()
        };
        let reached = |name: &String| {
            let outer = outer(name);
            self.unchecked.contains(&outer) || newer.unchecked.contains(&outer)
        };
        // Std functions are named by their module's path, `std/…::name`.
        let loaded = |name: &String| name.starts_with("std/");
        let only_one: Vec<&String> = (self.fns.keys().filter(|n| !newer.fns.contains_key(*n)))
            .chain(newer.fns.keys().filter(|n| !self.fns.contains_key(*n)))
            .collect();
        // Loaded in one version only: reached, even where lazy std checked all of it.
        let fact = Change::Fact("reached std");
        if only_one.iter().any(|n| loaded(n)) && !out.contains(&fact) {
            let at = self.facts.iter().position(|f| f.0 == "reached std");
            let before = |c: &Change| match c {
                Change::Fact(f) => self.facts.iter().position(|x| x.0 == *f) < at,
                Change::Fn(_) => false,
            };
            let i = out.iter().take_while(|c| before(c)).count();
            out.insert(i, fact);
        }
        let mut fns: Vec<Change> = (self.fns.iter())
            .filter(|(name, hashes)| newer.fns.get(*name).is_some_and(|h| h != *hashes))
            .map(|(name, _)| name)
            .chain(only_one.into_iter().filter(|name| !loaded(name)))
            .filter(|name| !reached(name))
            .map(|name| Change::Fn(name.clone()))
            .collect();
        fns.sort();
        out.extend(fns);
        out
    }

    /// The number of functions summarized.
    pub fn functions(&self) -> usize {
        self.fns.values().map(Vec::len).sum()
    }
}

/// Is `name` the qualified name of a closure in the function `outer` (at any depth)?
pub fn is_closure_of(name: &str, outer: &str) -> bool {
    name.strip_prefix(outer)
        .is_some_and(|rest| rest.starts_with("::{closure#"))
}
