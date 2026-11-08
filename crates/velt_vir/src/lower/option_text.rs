//! Printing and stringifying an option of a number, string or boolean (`number | undefined`,
//! `string | null`) as a branch on whether it holds a value, then the payload's own path: the
//! nothing's word (`undefined`, `null`) or what printing the payload writes. `console.log(x)`,
//! a template part, `String(x)` and a lone `${x}` take it; the format glue is not involved, so
//! such a value costs what its payload does plus the test. Other options print as before.

use velt_sema::hir::{Nothing, TyId, TyKind};

use super::FnLower;
use crate::vir::{Operand, Place, STR_AGG};

impl FnLower<'_, '_> {
    /// The payload and nothing of `t` when it is a one-nothing option of a number, string or
    /// boolean (not `T | null | undefined`, whose payload is itself an option).
    pub(super) fn option_scalar(&self, t: TyId) -> Option<(TyId, Nothing)> {
        let TyKind::Option(p, n) = self.cx.kind(t) else {
            return None;
        };
        let scalar = matches!(
            self.cx.kind(p),
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str
        );
        (scalar && n != Nothing::Both).then_some((p, n))
    }

    /// Branch on the option `v` (of type `t`): `some` with the payload's place, else `none`,
    /// both continuing after.
    pub(super) fn option_split(
        &mut self,
        v: Operand,
        t: TyId,
        some: impl FnOnce(&mut Self, Place),
        none: impl FnOnce(&mut Self),
    ) {
        let place = self.place_of(v, t);
        let has = self.option_is_some(&place, t);
        let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(has, yes, no);
        self.switch_to(yes);
        let payload = self.some_payload(&place, t);
        some(self, payload);
        self.goto(done);
        self.switch_to(no);
        none(self);
        self.goto(done);
        self.switch_to(done);
    }

    /// `console.log`'s text of the option `v` (`option_scalar`: payload `p`, nothing `n`).
    pub(super) fn write_option(
        &mut self,
        stream: &Operand,
        v: Operand,
        t: TyId,
        p: TyId,
        n: Nothing,
    ) {
        self.option_split(
            v,
            t,
            |lw, payload| lw.write_payload(stream, payload, p),
            |lw| lw.write_text(stream, n.word()),
        );
    }

    /// The option `v` appended to the builder at `buf` (a template part).
    pub(super) fn push_option(&mut self, buf: &Operand, v: Operand, t: TyId, p: TyId, n: Nothing) {
        self.option_split(
            v,
            t,
            |lw, payload| match lw.cx.kind(p) {
                TyKind::Str => {
                    let a = lw.addr(payload);
                    lw.push_str(buf, a);
                }
                _ => lw.push_scalar(buf, Operand::Copy(payload), p),
            },
            |lw| lw.push_text(buf, n.word()),
        );
    }

    /// `String(x)` of the option `v` into the string local `out`, for a float, boolean or
    /// string payload: its text as `stringify` makes it, or the nothing's word.
    pub(super) fn option_text_into(
        &mut self,
        v: Operand,
        t: TyId,
        (p, n): (TyId, Nothing),
        out: crate::vir::Local,
    ) {
        self.option_split(
            v,
            t,
            |lw, payload| lw.scalar_text_into(Operand::Copy(payload), p, out),
            |lw| {
                let text = lw.str_lit(n.word());
                let src = lw.operand_addr(text, crate::vir::Ty::Agg(STR_AGG));
                let o = lw.addr(Place::local(out));
                lw.call_rt(super::rt::Rt::StrClone, vec![src, o], None);
            },
        );
    }
}
