"""The deliberate breaks for #3288 (the whole unit's encoding), for mutants.py --list."""
U = "crates/velt_sema/src/units/codec/unit.rs"
MUTANTS = [
    ("checksum not checked", U, "    if u64::from_le_bytes(sum.try_into().ok()?) != checksum(rest) {",
     "    if u64::from_le_bytes(sum.try_into().ok()?) == checksum(rest) && false {"),
    ("no decode stack in decode_unit", U, "        super::with_decode_stack(crate::SEMA_STACK_BUDGET, || {",
     "        super::with_decode_stack(1 << 20, || {"),
    ("fn_sites moved to `empty`", U,
     ["        dyn_iface_targets, fn_views, fn_sites, promise_forwarders, nested, nested_locals,",
      "        jsx_providers, jsx_refusals, poisoned, added_defs,"],
     ["        dyn_iface_targets, fn_views, promise_forwarders, nested, nested_locals,",
      "        jsx_providers, jsx_refusals, poisoned, added_defs, fn_sites,"]),
]
