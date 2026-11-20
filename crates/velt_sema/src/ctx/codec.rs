//! The encoding of this module's types in a finished unit (`units::codec`): every field and
//! variant, so a new one fails to build.

use super::*;

crate::units::codec::structs! {
    ModuleScope {
        items, exports, namespaces, namespace_aliases, type_only, via_export_type, is_std,
        is_library, type_keys,
    }
}
crate::units::codec::enums! {
    @fielded Item {
        0 => Def(f0),
        1 => Alias(f0),
    }
}
