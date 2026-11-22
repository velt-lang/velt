//! The encoding of this module's types in a finished unit (`units::codec`): every field and
//! variant, so a new one fails to build.

use super::*;

crate::units::codec::structs! {
    Copies {
        copies, writes, seen, in_keys, in_optional, null_literals, record_writes, view_casts,
        view_pairs, proxies, dyn_sources, makes_proxies, frozen, spread_after, type_spreads,
    }
    Copy {
        from, to, fields, absent, span, source, fresh, in_order, generic, record,
    }
    Write {
        ty, name, span, dynamic, checkable,
    }
    Frozen {
        from, to, span, existing,
    }
}
crate::units::codec::enums! {
    Seen {
        0 => Printed,
        1 => Serialized,
        2 => Keys,
        3 => Identity,
        4 => Spread(f0),
        5 => In(f0),
        6 => FromDynamic,
        7 => Dynamic(f0),
        8 => Key,
    }
}
