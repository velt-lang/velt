//! Evidence of mutation in one checked body (for mutation inference, `super::mutation`):
//! which locals have their contents modified and which are assigned as a whole.
//!
//! - A place used `BorrowMut` (written through, or passed to a param / receiver that is
//!   modified) modifies the local it is rooted at; so does a closure capturing the local
//!   mutably.
//! - `x = v` / `x += v` on a local reassigns it (a param reassigned as a whole is a local
//!   rebinding, JS semantics); it does not modify the caller's value.
//! - Borrowing pattern bindings (`for...of` elements, `match` / destructuring bindings) point into
//!   the matched place: modifying the binding modifies the local that place is rooted at.
//! - A call through a function value may modify what its non-Copy arguments point to. This is
//!   *weak* evidence: an argument that lies in its root's own memory (no array element on the
//!   way, e.g. `p` or `p.inner`) marks the root as possibly written, so the root is never
//!   assumed read-only (`LocalDef::mutable`), but it does not make the root a modified param for
//!   callers: the call site that supplies the function value accounts for what it modifies
//!   (`super::patch`, callback rule). Array elements live in the array's heap buffer, not in the
//!   root. A closure capturing a variable weakly passes that on to the enclosing function.
//!
//! Evidence is flow-insensitive: a mutation on any path (one branch, a loop body, a closure)
//! counts.

use std::collections::{HashMap, HashSet};

use velt_common::Span;

use crate::body::places::place_root;
use crate::ctx::Ctx;
use crate::hir::{
    Block, Callee, Def, Expr, ExprKind as E, LocalId, PassMode, Pat, PatKind, Stmt, StmtKind as S,
    UseMode,
};
use crate::visit::{self, VisitMut};

/// What one body does to its locals.
#[derive(Default)]
pub(super) struct Evidence {
    /// Locals whose contents are modified in place.
    pub mutated: HashSet<LocalId>,
    /// Locals whose memory a function value may write (weak evidence, see the module docs).
    pub weak: HashSet<LocalId>,
    /// Locals assigned as a whole, with the first such assignment.
    pub reassigned: HashMap<LocalId, Span>,
}

/// Is local `l` modified before the body rebinds it for good? The rebinding is the first
/// top-level `l = v` statement (runs unconditionally); `None` without one. Modifications after
/// it only touch the callee's own value.
pub(super) fn modified_before_rebind(cx: &mut Ctx, b: &Block, l: LocalId) -> Option<bool> {
    let at = b.stmts.iter().position(|s| match &s.kind {
        S::Expr(Expr {
            kind: E::Assign { place, .. },
            ..
        }) => matches!(place.kind, E::Local(x, _) if x == l),
        _ => false,
    })?;
    let mut prefix = Block {
        stmts: b.stmts[..=at].to_vec(),
        value: None,
        span: b.span,
    };
    Some(collect(cx, &mut prefix).mutated.contains(&l))
}

/// Collect the evidence of body `b`.
pub(super) fn collect(cx: &mut Ctx, b: &mut Block) -> Evidence {
    let mut v = Collector {
        cx,
        ev: Evidence::default(),
        aliases: HashMap::new(),
        skip: HashSet::new(),
    };
    visit::block(b, &mut v);
    let Collector {
        mut ev, aliases, ..
    } = v;
    ev.mutated = through_aliases(&ev.mutated, &aliases, false);
    ev.weak = through_aliases(&ev.weak, &aliases, true);
    ev
}

/// `set` plus the roots its borrowing bindings point into (only through bindings into their
/// root's own memory when `in_memory`).
fn through_aliases(set: &HashSet<LocalId>, aliases: &Aliases, in_memory: bool) -> HashSet<LocalId> {
    let mut out = set.clone();
    for &start in set {
        let mut l = start;
        // Bounded: alias chains follow declaration order, so they cannot cycle.
        for _ in 0..aliases.len() {
            match aliases.get(&l) {
                Some(&(root, inside)) if root != l && (inside || !in_memory) => {
                    out.insert(root);
                    l = root;
                }
                _ => break,
            }
        }
    }
    out
}

/// Borrowing binding → (the local its matched place is rooted at, the place lies in that
/// local's own memory).
type Aliases = HashMap<LocalId, (LocalId, bool)>;

struct Collector<'a, 'm> {
    cx: &'a mut Ctx<'m>,
    ev: Evidence,
    aliases: Aliases,
    /// Whole-local assignment targets (not modifications of the local's contents).
    skip: HashSet<*const Expr>,
}

impl Collector<'_, '_> {
    fn bind(&mut self, p: &Pat, scrutinee: &Expr) {
        let Some(root) = place_root(scrutinee) else {
            return;
        };
        let inside = in_root_memory(scrutinee);
        let mut bindings = vec![];
        borrowing_bindings(p, &mut bindings);
        for l in bindings {
            self.aliases.insert(l, (root, inside));
        }
    }

    /// Arguments of a call through a function value (see the module docs).
    fn indirect_args(&mut self, args: &[Expr]) {
        for a in args {
            if self.cx.is_copy(a.ty) || !in_root_memory(a) {
                continue;
            }
            if let Some(l) = place_root(a) {
                self.ev.weak.insert(l);
            }
        }
    }
}

/// Is place `e` part of its root local's own memory (no array element on the way)?
pub(super) fn in_root_memory(e: &Expr) -> bool {
    match &e.kind {
        E::Local(..) => true,
        E::Field { base, .. } | E::UnwrapSome(base, _) | E::UnwrapVariant { expr: base, .. } => {
            in_root_memory(base)
        }
        _ => false,
    }
}

fn borrowing_bindings(p: &Pat, out: &mut Vec<LocalId>) {
    match &p.kind {
        PatKind::Binding(l, UseMode::Borrow | UseMode::BorrowMut) => out.push(*l),
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter().for_each(|q| borrowing_bindings(q, out))
        }
        PatKind::Array { elems, .. } => elems.iter().for_each(|q| borrowing_bindings(q, out)),
        PatKind::Adt { fields } => fields.iter().for_each(|(_, q)| borrowing_bindings(q, out)),
        PatKind::Some(q) => borrowing_bindings(q, out),
        _ => {}
    }
}

impl VisitMut for Collector<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            S::ForOf {
                binding,
                iter,
                consume: false,
                ..
            } => self.bind(binding, iter),
            S::LetPat { pat, init } => self.bind(pat, init),
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        if self.skip.remove(&(e as *const Expr)) {
            return;
        }
        match &e.kind {
            E::Assign { place, .. } | E::CompoundAssign { place, .. } => {
                if let E::Local(l, _) = place.kind {
                    self.ev.reassigned.entry(l).or_insert(e.span);
                    self.skip.insert(&**place as *const Expr);
                }
            }
            E::Local(l, UseMode::BorrowMut) => {
                self.ev.mutated.insert(*l);
            }
            E::Match { scrutinee, arms } => {
                for a in arms {
                    self.bind(&a.pat, scrutinee);
                }
            }
            E::Closure(def) => {
                if let Some(Def::Fn(c)) = &self.cx.defs[def.0 as usize] {
                    for cap in &c.captures {
                        let written = c.body.locals[cap.inner.0 as usize].mutable;
                        match cap.mode {
                            PassMode::BorrowMut => self.ev.mutated.insert(cap.outer),
                            PassMode::Borrow if written => self.ev.weak.insert(cap.outer),
                            _ => false,
                        };
                    }
                }
            }
            E::Call {
                callee: Callee::Indirect(_),
                args,
            } => self.indirect_args(args),
            _ => {}
        }
    }
}
