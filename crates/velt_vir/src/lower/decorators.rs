//! What lowering reads of `Program::decorators` (hir_encodings.md "Decorators"): the compile-time
//! decorators the compiler acts on, each found by its role in the hook table
//! (`velt_common::std_hooks`), as sema finds them (`velt_sema` `decorators/known.rs`), never by
//! the name a use is written with.

use velt_common::std_hook;
use velt_sema::hir::{self, DefId, TyId};

use super::{ice, Cx, FnLower};
use crate::vir::{Inline, Operand, Proj, Rvalue, Ty};

impl Cx<'_> {
    /// `@inline` / `@noInline` on function `def` (`Function::inline`): sema has checked that at
    /// most one is there, on a function that is neither async nor a generator. `@inline` is
    /// `Always` when it says `"always"`; its default, `"hint"`, offers the body to callers, which
    /// every body is while a program is one unit, so it is `Default` until units (#2943) carry
    /// bodies across (docs/internals/design/package-machine-code.md §1).
    pub(super) fn inline_of(&self, def: DefId) -> Inline {
        let uses = self.hir.decorators.get(&hir::DecoratorTarget::Def(def));
        for u in uses.map_or(&[][..], Vec::as_slice) {
            if let hir::Def::ExternFn(x) = self.hir.def(u.def) {
                match x.name.as_str() {
                    std_hook!(decorator_inline) => {
                        return match u.args.first() {
                            Some(hir::ConstTree::Str(how)) if how == "always" => Inline::Always,
                            _ => Inline::Default,
                        };
                    }
                    std_hook!(decorator_no_inline) => return Inline::Never,
                    _ => {}
                }
            }
        }
        Inline::Default
    }

    /// `@cold` on function `def` (`Function::rare`): sema has checked it is on a function with a
    /// body that is neither async nor a generator, and not `@inline`.
    pub(super) fn is_rare(&self, def: DefId) -> bool {
        let uses = self.hir.decorators.get(&hir::DecoratorTarget::Def(def));
        (uses.map_or(&[][..], Vec::as_slice).iter()).any(|u| {
            matches!(self.hir.def(u.def), hir::Def::ExternFn(x) if x.name == std_hook!(decorator_cold))
        })
    }
}

impl Cx<'_> {
    /// Whether struct `def` is `@repr("transparent")` (sema checked it is a struct of one field,
    /// neither optional nor `void`).
    fn transparent(&self, def: DefId) -> bool {
        let uses = self.hir.decorators.get(&hir::DecoratorTarget::Def(def));
        (uses.map_or(&[][..], Vec::as_slice).iter()).any(|u| {
            matches!(self.hir.def(u.def), hir::Def::ExternFn(x) if x.name == std_hook!(decorator_repr))
        })
    }

    /// The type of the one field of `t`, when `t` is a `@repr("transparent")` struct.
    pub(super) fn transparent_field(&mut self, t: TyId) -> Option<TyId> {
        let hir::TyKind::Adt(d, _) = self.kind(t) else {
            return None;
        };
        if !self.transparent(d) {
            return None;
        }
        match self.adt_field_tys(t)[..] {
            [f] => Some(f),
            _ => ice("a transparent struct has one field"),
        }
    }

    /// `@align(n)` on struct `def`: the alignment its values get (sema checked `n` is a power of
    /// two from 1 to 4096). Layouts raise theirs to it, never lower it (`layout.rs`).
    pub(super) fn align_of(&self, def: DefId) -> Option<u32> {
        let uses = self.hir.decorators.get(&hir::DecoratorTarget::Def(def))?;
        uses.iter()
            .find_map(|u| match (self.hir.def(u.def), u.args.first()) {
                (hir::Def::ExternFn(x), Some(hir::ConstTree::Int(n)))
                    if x.name == std_hook!(decorator_align) =>
                {
                    u32::try_from(*n).ok()
                }
                _ => None,
            })
    }
}

impl FnLower<'_, '_> {
    /// Value `v` of type `t` as C takes it: the field of a `@repr("transparent")` struct (of a
    /// transparent struct of …), with its type; anything else as it is.
    pub(super) fn unwrap_transparent(&mut self, mut v: Operand, mut t: TyId) -> (Operand, TyId) {
        while let Some(field) = self.cx.transparent_field(t) {
            let agg = self.cx.ty(t);
            let mut place = self.operand_place(v, agg);
            place.proj.push(Proj::Field(0));
            (v, t) = (Operand::Copy(place), field);
        }
        (v, t)
    }

    /// Value `v`, as C gives it, as a value of type `t`: a `@repr("transparent")` struct (of a
    /// transparent struct of …) built around it; anything else as it is.
    pub(super) fn wrap_transparent(&mut self, v: Operand, t: TyId) -> Operand {
        let Some(field) = self.cx.transparent_field(t) else {
            return v;
        };
        let inner = self.wrap_transparent(v, field);
        match self.cx.ty(t) {
            Ty::Agg(agg) => self.rvalue_temp(Ty::Agg(agg), Rvalue::Aggregate(agg, vec![inner])),
            _ => ice("a transparent struct is an aggregate"),
        }
    }
}
