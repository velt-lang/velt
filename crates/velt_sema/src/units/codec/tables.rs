//! Encodings of foreign types the saved side tables hold (#2943 M2). The side tables' own types
//! are encoded next to their definitions (`crate::units::codec::structs!` blocks at the end of
//! their modules), where their private fields are visible.

use std::mem::Discriminant;

use super::{Codec, Reader, Writer};
use crate::hir::TyKind;

/// A type kind with tag `tag` and every field zero (what decoding the tag alone gives).
fn kind_with_tag(tag: u8) -> Option<TyKind> {
    let mut bytes = [0u8; 64];
    bytes[0] = tag;
    let mut r = Reader::new(&bytes, &[]);
    let k = TyKind::dec(&mut r);
    (!r.failed).then_some(k)
}

/// The variant of a type kind (`ownership::local_async::values::Head::Kind`), as its tag.
impl Codec for Discriminant<TyKind> {
    fn enc(&self, w: &mut Writer) {
        let tag = (0..=u8::MAX)
            .find(|&t| kind_with_tag(t).is_some_and(|k| std::mem::discriminant(&k) == *self))
            .expect("ICE: every type kind has a tag");
        w.byte(tag);
    }
    fn dec(r: &mut Reader) -> Self {
        match kind_with_tag(r.byte()) {
            Some(k) => std::mem::discriminant(&k),
            None => {
                r.fail();
                std::mem::discriminant(&TyKind::Unit)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// The side tables' enums listed `@fielded` end from zeros, so damaged input ends in each
    /// (those private to their modules have this test beside them).
    #[test]
    fn side_table_enums_end_on_damaged_input() {
        macro_rules! ends {
            ($($t:ty),* $(,)?) => {$(
                assert!(crate::units::codec::ends_from_zeros::<$t>(), stringify!($t));
            )*};
        }
        ends!(
            crate::copy_flow::Store,
            crate::ctx::Item,
            crate::iface_writes::SetterSrc,
            crate::symbols::KnownSymbol,
            crate::synth_ifaces::Form,
            crate::type_defaults::DefaultsOf,
        );
    }
}
