//! The encoding of this module's types in a finished unit (`units::codec`): every field and
//! variant, so a new one fails to build.

use super::*;

crate::units::codec::structs! {
    Live {
        check, seeded, live, todo, types, adts, methods, impls, kinds, ifaces, gated, opened,
        waiting, texts, dynamic_classes, finishing, present, assumed, late, demanded, int_text,
        prints_float, prints_containers, tasks, module_state, units, units_loaded,
    }
}
crate::units::codec::enums! {
    @fielded Gate {
        0 => Adt(f0),
        1 => Iface(f0),
        2 => Kind(f0),
    }
}

/// `Gate` is listed `@fielded`: it ends from zeros, so damaged input ends in it.
#[cfg(test)]
#[test]
fn gate_ends_on_damaged_input() {
    assert!(crate::units::codec::ends_from_zeros::<Gate>());
}
