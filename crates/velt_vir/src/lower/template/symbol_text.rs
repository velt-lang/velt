//! JS's `ToString` of a value that may be a symbol, where it throws (`TemplateToString`, a
//! template literal part whose type parameter is `symbol` or a union holding one; elements of
//! an array being joined use [`SYMBOL_TO_STRING`] too): Velt panics with JS's `TypeError`.

use velt_sema::hir::{TyId, TyKind};

use super::super::FnLower;
use crate::vir::{Operand, Place};

/// The panic of a symbol converted to a string implicitly (an element of an array being joined,
/// a generic template part), JS's `TypeError` message.
pub(super) const SYMBOL_TO_STRING: &str = "TypeError: Cannot convert a Symbol value to a string";

impl FnLower<'_, '_> {
    /// JS's `ToString` of the value at `place` when `ty` may hold a symbol: `symbol` itself (it
    /// throws), `symbol | null`, or a union with a `symbol` member (tested at run time: the
    /// active member is written as `${x}` writes it, a symbol throws). False for other types,
    /// which [`push_formatted`](Self::push_formatted) writes as before.
    pub(super) fn push_symbol_text(&mut self, buf: &Operand, place: &Place, ty: TyId) -> bool {
        if !self.holds_symbol(ty, 0) {
            return false;
        }
        self.push_implicit_text(buf, place, ty);
        true
    }

    /// Can a value of type `ty` be a symbol (`symbol`, or one in an optional or a union)?
    fn holds_symbol(&mut self, ty: TyId, depth: u32) -> bool {
        if depth > 16 {
            return false;
        }
        match self.cx.kind(ty) {
            TyKind::Symbol => true,
            TyKind::Option(e) => self.holds_symbol(e, depth + 1),
            TyKind::Adt(d, _) if self.cx.is_union(ty) => {
                let n = self.cx.enum_def(d).variants.len() as u32;
                (0..n).any(|v| {
                    self.cx
                        .variant_tys(ty, v)
                        .into_iter()
                        .any(|t| self.holds_symbol(t, depth + 1))
                })
            }
            _ => false,
        }
    }

    /// `${x}` of the value at `place` (a type [`Self::holds_symbol`] accepts, or a member of
    /// one): a symbol throws JS's `TypeError`, `null` is `"null"`, a union writes its active
    /// member.
    fn push_implicit_text(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        match self.cx.kind(ty) {
            TyKind::Symbol => self.panic_msg(SYMBOL_TO_STRING),
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool => {
                self.push_scalar(buf, Operand::Copy(place.clone()), ty)
            }
            TyKind::Never => {}
            TyKind::Option(e) => {
                let (some_bb, none_bb, done) =
                    (self.new_block(), self.new_block(), self.new_block());
                let some = self.option_is_some(place, ty);
                self.branch(some, some_bb, none_bb);
                self.switch_to(some_bb);
                let payload = self.some_payload(place, ty);
                self.push_implicit_text(buf, &payload, e);
                self.goto(done);
                self.switch_to(none_bb);
                self.push_text(buf, "null");
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Adt(..) if self.cx.is_union(ty) => {
                self.for_each_variant(place, ty, |lw, v, parts| {
                    if let Some(l) = lw.variant_literal(ty, v) {
                        lw.push_literal(buf, &l, false);
                    }
                    for (pp, pt) in parts {
                        lw.push_implicit_text(buf, &pp, pt);
                    }
                });
            }
            _ => {
                if !self.push_js_object(buf, place, ty) {
                    self.push_js_string(buf, place, ty);
                }
            }
        }
    }
}
