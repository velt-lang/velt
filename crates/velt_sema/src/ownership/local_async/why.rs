//! Why a value crosses a thread boundary ([`super::graph`]): the boundary, where it is, and the
//! type carrying the value there, with the order in which reasons are preferred.

use velt_common::Span;

use crate::hir::TyId;

use super::graph::Site;

/// Where a value leaves the task that made it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Boundary {
    /// Spawned: a closure literal, the callee or an argument of a spawned call.
    Spawn,
    /// `shared(...)`.
    Shared,
    /// The value of a `Mutex`, which other threads lock once it is shared.
    Mutex,
    /// A channel send.
    Channel,
    /// A value that settles a promise, which another task may await.
    Settle,
}

/// Why a value crosses: the boundary, where (in the program when the boundary is in a library, std
/// or a package it depends on: the call into it), and the type that carries it there when the value
/// is reached through the heap.
#[derive(Clone, Copy, Debug)]
pub(super) struct Why {
    pub span: Span,
    pub boundary: Boundary,
    /// `span` is in a library (std or a package the program depends on).
    pub library: bool,
    /// The boundary is in a library, reached through the program's call at `span`: a library
    /// function that hands a callback to other threads runs a copy of it on each (a server's
    /// handler, a connection per task).
    pub in_library: bool,
    /// A value of this type reaches the boundary, and the closure is stored in it.
    pub via: Option<TyId>,
    /// While the reason is inside a library (`library`): how many calls deep the path from the
    /// boundary is in functions it entered through their results (`Node::Ret`), whose callers are
    /// that many calls up the path ([`Why::step`]).
    pub depth: u8,
    /// `shared(...)` of a value holding the function, not of the function itself.
    pub in_value: bool,
    /// The value was made a dynamic one at this conversion, to this type (`Graph::dynamic`), and
    /// crosses because a value of a dynamic type does (#2865): not through a holder's type.
    pub made_dyn: Option<(Span, TyId)>,
}

impl Why {
    /// The order in which reasons are preferred: one in user code, the earliest, and between
    /// two at one place the holder whose name sorts first, so that the choice never depends on
    /// how types happen to be numbered.
    pub(super) fn rank(&self, cx: &crate::ctx::Ctx) -> (bool, u32, u32, u8, String) {
        let via = self.via.map(|t| cx.display(t)).unwrap_or_default();
        (
            self.library,
            self.span.file.0,
            self.span.lo,
            self.depth,
            via,
        )
    }

    /// `self.rank(cx) < other.rank(cx)`, naming the holders only when the rest is equal.
    pub(super) fn before(&self, other: &Why, cx: &crate::ctx::Ctx) -> bool {
        let key = |w: &Why| (w.library, w.span.file.0, w.span.lo, w.depth);
        match key(self).cmp(&key(other)) {
            std::cmp::Ordering::Equal if self.via != other.via => self.rank(cx) < other.rank(cx),
            o => o.is_lt(),
        }
    }

    /// `self`, followed from a node to a value flowing into it: `into_ret` when that value is
    /// what a function returns (the path enters the function from its call), `site` the call
    /// passing it when it is an argument (the path leaves a function for its caller). A boundary
    /// inside a library is reported at the program's call into it. `None` for a path no run
    /// takes: one that entered a function through its result, from a call in the library, and
    /// leaves it through a parameter for the program, a caller of another call of it. The graph
    /// has one node per function, not per call, so without this a library helper that the
    /// boundary's code and the program both call (a result flowing to the boundary) would take
    /// what the program passes to it across too.
    pub(super) fn step(self, into_ret: bool, site: Site) -> Option<Why> {
        if !self.library {
            return Some(self);
        }
        let w = match site {
            Some((span, false)) if self.depth == 0 => Why {
                span,
                library: false,
                in_library: true,
                ..self
            },
            Some((_, false)) => return None,
            Some((_, true)) => Why {
                depth: self.depth.saturating_sub(1),
                ..self
            },
            None => self,
        };
        Some(match into_ret && w.library {
            true => Why {
                depth: w.depth.saturating_add(1),
                ..w
            },
            false => w,
        })
    }

    /// `self` for the values a spawned call's function returns: the path starts inside it, one
    /// call deep.
    pub(super) fn entered(self) -> Why {
        match self.library {
            true => Why { depth: 1, ..self },
            false => self,
        }
    }
}
