//! Which field initializers a resolution of throw sources has counted ([`ThrowSrc::Defaults`]).
//!
//! Initializers can construct their own class with ever larger type arguments (`Box<T>` running
//! `new Box<Box<T>>()`), which would never end. An instantiation *grows* when one of its type
//! arguments contains, as a proper part, a type argument of an instantiation of the same class
//! it is resolved inside (`Box<Box<E>>` inside `Box<E>`). A class whose initializers grow
//! constructs itself without end, so growth only ever comes from such programs: it is limited
//! to [`MAX_GROWTH`] instantiations per class and resolution, which also keeps initializers
//! growing in several ways at once (whose instantiations multiply) fast. Other instantiations — siblings such as `new Box<E1>()`
//! … `new Box<E70>()` in one `try` (#372) — all count; there are finitely many of them, since
//! their arguments come from the program's types and from growth, and each counts once.
//!
//! [`ThrowSrc::Defaults`]: crate::defs::ThrowSrc::Defaults

use std::collections::{HashMap, HashSet};

use crate::hir::{DefId, TyId};
use crate::types::{children, Types};

/// How many growing instantiations of one class a resolution counts.
const MAX_GROWTH: usize = 16;

/// The counted instantiations, keyed on the class and its type arguments (`Box<E1>` and
/// `Box<E2>` throw different errors), and the chain being resolved.
#[derive(Default)]
pub(crate) struct InitsSeen {
    seen: HashMap<DefId, HashSet<Vec<TyId>>>,
    /// The instantiations the current resolution is inside, innermost last.
    chain: Vec<(DefId, Vec<TyId>)>,
    growth: HashMap<DefId, usize>,
}

impl InitsSeen {
    /// Enter class `d`'s initializers with type arguments `args`: false when they are counted
    /// already or would grow past the limits. After true, the caller counts the initializers'
    /// sources and then calls [`leave`](Self::leave).
    pub(crate) fn enter(&mut self, ty: &Types, d: DefId, args: &[TyId]) -> bool {
        if self.seen.get(&d).is_some_and(|s| s.contains(args)) {
            return false;
        }
        let grows = self
            .chain
            .iter()
            .any(|(e, prev)| *e == d && contains_any(ty, args, prev));
        if grows {
            let total = self.growth.entry(d).or_default();
            if *total >= MAX_GROWTH {
                return false;
            }
            *total += 1;
        }
        self.seen.entry(d).or_default().insert(args.to_vec());
        self.chain.push((d, args.to_vec()));
        true
    }

    /// Leave the initializers entered last.
    pub(crate) fn leave(&mut self) {
        self.chain.pop();
    }
}

/// Does a type in `args` contain one of `prev` as a proper part?
fn contains_any(ty: &Types, args: &[TyId], prev: &[TyId]) -> bool {
    let mut stack: Vec<TyId> = args.iter().flat_map(|a| children(ty.kind(*a))).collect();
    while let Some(t) = stack.pop() {
        if prev.contains(&t) {
            return true;
        }
        stack.extend(children(ty.kind(t)));
    }
    false
}
