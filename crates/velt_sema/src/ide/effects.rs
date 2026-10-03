//! What each function, method, constructor and named closure does to its caller beyond its
//! result, as inference decided it: what it throws (`throws` pass) and what it modifies
//! (ownership inference: a `this` or parameter passed `BorrowMut`). Captured once by the
//! snapshot so editors can show inferred facts the source does not spell out.

use std::collections::HashMap;

use velt_common::Span;

use super::display::Names;
use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnInfo, FnKind};
use crate::hir::{DefId, PassMode, TyKind};

/// What a function modifies (docs/reference/memory.md, "Mutation is inferred").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mutation {
    /// A method that modifies `this` (directly, through a field, a callee or a closure); never
    /// a constructor.
    pub this: bool,
    /// Parameters whose contents it modifies, in declaration order.
    pub params: Vec<String>,
}

impl Mutation {
    /// Does it modify anything its caller can see?
    pub fn any(&self) -> bool {
        self.this || !self.params.is_empty()
    }
}

/// The inferred effects of one function.
pub(crate) struct Effects {
    /// What it throws, spelled as source (`None`: nothing).
    pub throws: Option<String>,
    /// `None` for closures: their parameters are passed by a fixed convention (callbacks may
    /// modify what they receive), not inferred.
    pub mutation: Option<Mutation>,
}

/// Effects of every function with a name in the source, by its declaring identifier (named
/// closures by the identifier of the variable they initialize, see `closures`).
pub(crate) fn capture(
    cx: &Ctx,
    names: &Names,
    closures: &HashMap<Span, DefId>,
) -> HashMap<Span, Effects> {
    let mut out = HashMap::new();
    for info in &cx.info {
        if let DefInfo::Fn(f) = info {
            if f.source.is_some() && f.name_span != Span::DUMMY {
                out.insert(f.name_span, effects(f, names, true));
            }
        }
    }
    for (decl, d) in closures {
        if let Some(f) = cx.try_fn(*d) {
            out.insert(*decl, effects(f, names, false));
        }
    }
    out
}

fn effects(f: &FnInfo, names: &Names, inferred_modes: bool) -> Effects {
    let throws = f
        .throws
        .filter(|t| !matches!(names.table.kind(*t), TyKind::Never))
        .map(|t| names.show_in(t, &f.generics.names));
    let mutation = inferred_modes.then(|| Mutation {
        // A constructor's `this` is the object it builds, not something its caller sees change.
        this: f.kind != FnKind::Ctor
            && f.this
                .as_ref()
                .is_some_and(|t| t.mode == PassMode::BorrowMut),
        params: f
            .params
            .iter()
            .filter(|p| p.mode == PassMode::BorrowMut)
            .map(|p| p.name.clone())
            .collect(),
    });
    Effects { throws, mutation }
}
