//! Types independent of one check's type table: the form an inferred error type is kept in
//! between the checks of one program (`fn_types`).

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{DefId, Nothing, TyId, TyKind};

/// A type independent of one check's type table.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Portable {
    /// A type without component types (`string`, `i32`, a literal type).
    Leaf(TyKind),
    /// A class (or other definition) instance: its id, declaration span and type arguments.
    Def(DefId, Span, Vec<Portable>),
    /// A value of an interface type (`unknown`, `any`, a thrown interface value): the
    /// interface's id, declaration span and type arguments.
    Dyn(DefId, Span, Vec<Portable>),
    /// An array, read-only (`readonly T[]`, #3061) or not.
    Array(Box<Portable>, bool),
    Option(Box<Portable>, Nothing),
}

impl Portable {
    /// How many types it is made of.
    fn size(&self) -> usize {
        match self {
            Portable::Leaf(_) => 1,
            Portable::Def(_, _, args) | Portable::Dyn(_, _, args) => {
                1 + args.iter().map(Portable::size).sum::<usize>()
            }
            Portable::Array(e, _) | Portable::Option(e, _) => 1 + e.size(),
        }
    }
}

impl Ctx<'_> {
    /// `t` as a [`Portable`] type kept in an inferred error set: as in JavaScript any thrown
    /// value (`null`, a string, a class, `Error | null`), but only a small type, since the sets
    /// reach every call through their sites.
    pub(super) fn port_thrown(&self, t: TyId) -> Option<Portable> {
        let p = self.port(t, 0)?;
        (p.size() <= 8).then_some(p)
    }

    /// `t` as a [`Portable`] type, for the seeds of another check.
    pub(super) fn port_type(&self, t: TyId) -> Option<Portable> {
        self.port(t, 0)
    }

    /// `t` as a [`Portable`] type (`None`: it has parts that only exist in this check).
    pub(super) fn port(&self, t: TyId, depth: u32) -> Option<Portable> {
        if depth > 16 {
            return None;
        }
        let k = self.ty.kind(t);
        Some(match k {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Unit
            | TyKind::Symbol
            | TyKind::Never
            | TyKind::Literal(_) => Portable::Leaf(k.clone()),
            TyKind::Adt(d, args) if self.collected(*d) => {
                let span = *self.def_spans.get(d.0 as usize)?;
                let args = (args.iter())
                    .map(|a| self.port(*a, depth + 1))
                    .collect::<Option<Vec<_>>>()?;
                Portable::Def(*d, span, args)
            }
            TyKind::Dyn(d, args) if self.collected(*d) => {
                let span = *self.def_spans.get(d.0 as usize)?;
                let args = (args.iter())
                    .map(|a| self.port(*a, depth + 1))
                    .collect::<Option<Vec<_>>>()?;
                Portable::Dyn(*d, span, args)
            }
            TyKind::Array(e) => Portable::Array(
                Box::new(self.port(*e, depth + 1)?),
                self.ty.is_readonly_array(t),
            ),
            TyKind::Option(e, n) => Portable::Option(Box::new(self.port(*e, depth + 1)?), *n),
            _ => return None,
        })
    }

    /// A [`Portable`] type in this check (`None`: its class is not the same here).
    pub(super) fn unport(&mut self, p: &Portable) -> Option<TyId> {
        let k = match p {
            Portable::Leaf(k) => k.clone(),
            Portable::Def(d, span, args) => {
                if self.def_spans.get(d.0 as usize) != Some(span) {
                    return None;
                }
                let args = (args.iter())
                    .map(|a| self.unport(a))
                    .collect::<Option<Vec<_>>>()?;
                TyKind::Adt(*d, args)
            }
            Portable::Dyn(d, span, args) => {
                if self.def_spans.get(d.0 as usize) != Some(span) {
                    return None;
                }
                let args = (args.iter())
                    .map(|a| self.unport(a))
                    .collect::<Option<Vec<_>>>()?;
                TyKind::Dyn(*d, args)
            }
            Portable::Array(e, true) => {
                let e = self.unport(e)?;
                return Some(self.ty.readonly_array(e));
            }
            Portable::Array(e, false) => TyKind::Array(self.unport(e)?),
            Portable::Option(e, n) => TyKind::Option(self.unport(e)?, *n),
        };
        Some(self.ty.intern(k))
    }
}

// The encoding of a finished unit (`units::codec`): every variant, so a new one fails to build.
crate::units::codec::enums! {
    Portable {
        0 => Leaf(f0),
        1 => Def(f0, f1, f2),
        2 => Dyn(f0, f1, f2),
        3 => Array(f0, f1),
        4 => Option(f0, f1),
    }
}
