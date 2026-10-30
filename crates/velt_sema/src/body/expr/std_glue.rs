//! Std-only intrinsics with extra rules: `__intrinsic_json_parse` throws the prelude's
//! `JsonError`. What JSON glue can be generated for is checked after all bodies
//! (`crate::json`).

use velt_common::Span;

use crate::body::FnCx;
use crate::defs::ThrowSrc;

impl FnCx<'_, '_> {
    /// A decoding failure surfaces as a thrown `JsonError`.
    pub(super) fn json_parse_throws(&mut self, span: Span) {
        match self.cx.json_error_ty() {
            Some(t) => self.throw_src(ThrowSrc::Direct(t, span)),
            None => self.cx.err(
                "`JsonError` is missing from the prelude (std/prelude/json/json.vlt)",
                span,
            ),
        }
    }
}
