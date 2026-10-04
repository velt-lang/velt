//! What one call argument does with places while the call runs: the places it hands to the
//! callee (`direct` uses: borrowed, mutably borrowed or moved arguments, and the variables a
//! closure argument captures) and the places it mutates or moves while it is being evaluated
//! (`nested` uses, e.g. `g(xs)` modifying `xs` inside `f(xs[0], g(xs))`).
//!
//! Reading a Copy value (`xs.length`, `xs[i]` of numbers, `p.x`) is not a use here: the value
//! is copied out before the call starts, so it can't alias anything the callee sees.

use std::collections::{HashMap, HashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{
    Block, Def, DefId, Expr, ExprKind as E, LocalDef, LocalId, PassMode, Stmt, UseMode,
};
use crate::visit::{self, VisitMut};

use crate::ownership::validate::place_text;

/// One step from a place to a part of it. `Index` stands for any element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Proj {
    Field(u32),
    Index,
}

/// A memory location rooted at a local of the function being checked.
#[derive(Clone, Debug)]
pub(super) struct Place {
    pub root: LocalId,
    pub proj: Vec<Proj>,
}

impl Place {
    /// Can the two places share memory? One must be a prefix of the other; distinct fields
    /// are disjoint, distinct indices are not (they may be equal at run time).
    pub fn overlaps(&self, other: &Place) -> bool {
        self.root == other.root
            && self.proj.iter().zip(&other.proj).all(|pair| match pair {
                (Proj::Field(a), Proj::Field(b)) => a == b,
                _ => true,
            })
    }
}

/// How a use reaches a place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Access {
    Shared,
    Unique,
    Move,
}

fn access_of(m: UseMode) -> Option<Access> {
    match m {
        UseMode::Copy => None,
        UseMode::Borrow => Some(Access::Shared),
        UseMode::BorrowMut => Some(Access::Unique),
        UseMode::Move => Some(Access::Move),
    }
}

/// A place used by an argument; `text` names it in diagnostics, `captured` says the use is a
/// closure's capture (`span` is then the closure's).
#[derive(Clone, Debug)]
pub(super) struct Use {
    pub place: Place,
    pub access: Access,
    pub span: Span,
    pub text: String,
    pub captured: bool,
}

/// Borrowing pattern bindings (`for...of` elements, `match` / destructuring bindings) point
/// into the matched place: `aliases[binding]` is that place, so `f(xs, x)` (modifying `xs`) inside
/// `for (const x of xs)` is seen as a use of `xs[..]`.
pub(super) type Aliases = HashMap<LocalId, Place>;

pub(super) struct Collector<'a, 'm> {
    pub cx: &'a Ctx<'m>,
    pub locals: &'a [LocalDef],
    pub aliases: &'a Aliases,
}

impl Collector<'_, '_> {
    /// The place `e` denotes and how its outermost node is used; `None` for non-places.
    pub fn place_of(&self, e: &Expr) -> Option<(Place, UseMode)> {
        match &e.kind {
            E::Local(l, m) => Some((self.root(*l), *m)),
            E::Field { base, index, mode } => {
                let (mut p, _) = self.place_of(base)?;
                p.proj.push(Proj::Field(*index));
                Some((p, *mode))
            }
            E::Index { base, mode, .. } => {
                let (mut p, _) = self.place_of(base)?;
                p.proj.push(Proj::Index);
                Some((p, *mode))
            }
            // An option's (a union's) payload lives in the value itself.
            E::UnwrapSome(base, mode)
            | E::UnwrapVariant {
                expr: base, mode, ..
            } => self.place_of(base).map(|(p, _)| (p, *mode)),
            // The same object, seen as a subclass.
            E::Downcast(base) => self.place_of(base),
            _ => None,
        }
    }

    fn root(&self, l: LocalId) -> Place {
        self.aliases.get(&l).cloned().unwrap_or(Place {
            root: l,
            proj: vec![],
        })
    }

    pub fn text(&self, e: &Expr) -> String {
        place_text(self.cx, self.locals, e)
    }

    /// Uses of argument `e` that last for the whole call (`direct`) and those that happen
    /// while it is evaluated (`nested`).
    pub fn direct(&self, e: &mut Expr, direct: &mut Vec<Use>, nested: &mut Vec<Use>) {
        if let Some((place, mode)) = self.place_of(e) {
            if let Some(access) = access_of(mode) {
                let text = self.text(e);
                direct.push(Use {
                    place,
                    access,
                    span: e.span,
                    text,
                    captured: false,
                });
            }
            return self.index_operands(e, nested);
        }
        match &mut e.kind {
            E::Upcast(x) | E::Downcast(x) | E::WrapSome(x) | E::ToDyn { expr: x, .. } => {
                self.direct(x, direct, nested)
            }
            E::If { cond, then, els } => {
                self.nested(cond, nested);
                self.direct(then, direct, nested);
                self.direct(els, direct, nested);
            }
            E::Closure(def) => self.captures(*def, e.span, false, direct),
            _ => self.nested(e, nested),
        }
    }

    /// Index operands inside place `e` (`i` in `xs[i].f`) are evaluated, not passed.
    fn index_operands(&self, e: &mut Expr, nested: &mut Vec<Use>) {
        match &mut e.kind {
            E::Field { base, .. }
            | E::UnwrapSome(base, _)
            | E::UnwrapVariant { expr: base, .. }
            | E::Downcast(base) => self.index_operands(base, nested),
            E::Index { base, index, .. } => {
                self.nested(index, nested);
                self.index_operands(base, nested);
            }
            _ => {}
        }
    }

    /// Places mutated or moved anywhere inside `e` (assignments, modified arguments and moves of
    /// nested calls, closures that mutate or take captured variables).
    pub fn nested(&self, e: &mut Expr, out: &mut Vec<Use>) {
        let mut v = Nested {
            col: self,
            out,
            skip: HashSet::new(),
        };
        visit::expr(e, &mut v);
    }

    /// Places mutated or moved anywhere in block `b` (see [`nested`](Self::nested)).
    pub fn nested_block(&self, b: &mut Block, out: &mut Vec<Use>) {
        let mut v = Nested {
            col: self,
            out,
            skip: HashSet::new(),
        };
        visit::block(b, &mut v);
    }

    /// Places mutated or moved anywhere in `stmts` and the block value `value`.
    pub fn nested_stmts(&self, stmts: &mut [Stmt], value: Option<&mut Expr>, out: &mut Vec<Use>) {
        let mut v = Nested {
            col: self,
            out,
            skip: HashSet::new(),
        };
        for s in stmts {
            visit::stmt(s, &mut v);
        }
        if let Some(e) = value {
            visit::expr(e, &mut v);
        }
    }

    /// The variables closure `def` captures, as uses at `span` (only the mutating / moving
    /// captures when `mutating_only`).
    fn captures(&self, def: DefId, span: Span, mutating_only: bool, out: &mut Vec<Use>) {
        let Some(Def::Fn(c)) = &self.cx.defs[def.0 as usize] else {
            return;
        };
        for cap in &c.captures {
            let access = match cap.mode {
                PassMode::Copy => continue,
                PassMode::Borrow if mutating_only => continue,
                PassMode::Borrow => Access::Shared,
                PassMode::BorrowMut => Access::Unique,
                PassMode::Owned => Access::Move,
            };
            out.push(Use {
                place: self.root(cap.outer),
                access,
                span,
                text: self.locals[cap.outer.0 as usize].name.clone(),
                captured: true,
            });
        }
    }
}

/// Visitor behind [`Collector::nested`]. Only the outermost node of a place counts (its bases
/// carry derived modes), so bases are remembered in `skip` when their place is seen.
struct Nested<'c, 'a, 'm> {
    col: &'c Collector<'a, 'm>,
    out: &'c mut Vec<Use>,
    skip: HashSet<*const Expr>,
}

impl Nested<'_, '_, '_> {
    fn skip_bases(&mut self, e: &Expr) {
        if let E::Field { base, .. }
        | E::Index { base, .. }
        | E::UnwrapSome(base, _)
        | E::UnwrapVariant { expr: base, .. }
        | E::Downcast(base) = &e.kind
        {
            self.skip.insert(&**base as *const Expr);
        }
    }

    fn record(&mut self, e: &Expr, place: Place, access: Access) {
        let text = self.col.text(e);
        self.out.push(Use {
            place,
            access,
            span: e.span,
            text,
            captured: false,
        });
    }
}

impl VisitMut for Nested<'_, '_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        if self.skip.remove(&(e as *const Expr)) {
            return self.skip_bases(e);
        }
        match &e.kind {
            E::Assign { place, .. } | E::CompoundAssign { place, .. } => {
                if let Some((p, _)) = self.col.place_of(place) {
                    self.record(place, p, Access::Unique);
                    self.skip.insert(&**place as *const Expr);
                }
            }
            E::Closure(def) => self.col.captures(*def, e.span, true, self.out),
            _ => {
                if let Some((p, mode)) = self.col.place_of(e) {
                    if matches!(mode, UseMode::BorrowMut | UseMode::Move) {
                        let access = access_of(mode).expect("ICE: mutating use has an access");
                        self.record(e, p, access);
                    }
                    self.skip_bases(e);
                }
            }
        }
    }
}
