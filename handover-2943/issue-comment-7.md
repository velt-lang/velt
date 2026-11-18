Follow-up (mac-e688), M3: `velt build` from std's unit, debug goldens.

**4234 files: 85 failures in 50 files**, down from 92 in 56 at the last update. No file that passed before fails now.

Fixed on `units-passes`:
- **D1, vtable slots (8f4e92d70, 1a8efb18b):** std's unit gives every method and accessor of `Error` and its std subclasses a slot, whether or not something overrides it. A subclass's `get name()` or `toString` now runs from std's own code (`error_name_getter`, `error_text_values`, `error_tostring_override`).
  - Two regressions along the way, both fixed. Other std classes' `toString`/`toJSON` got slots that lowering never builds vtables for (`Date`, `Duration`). And with a dependency in the first unit, a dependency's `Error` subclass made `Error` be laid out again without the slots.
  - The run-time cost (the plan's limit is 3%) is still to be measured before units become the default.
- **D2, `any`'s member slots (6d592a039, 3c847b900):** recorded per std function, and the program has them when it reaches one. Two files with shape checks on `any` that had stopped running now pass again: `iface_any_args_write_check` and `any_to_union_object_member_panics`.
  - "Reaches" needs lazy std's live set. Std's unit has every std module loaded, while lazy std loads 109 to 190 of the ~830 for these goldens. The units live set now loads modules the way lazy std does: the program's modules, their direct std imports and the prelude first, then each module live code reaches into.
- **Check cost (76c6fc310):** a plain `velt check` no longer runs that live walk.

Opt-in experiment (`VELT_UNITS_WALK_STD=1`, c621d3fda): the async-closure analysis walks the std code the program reaches, not std's whole-std summaries. That fixes `record_symbol_accessors`. Two stream tests still fail: std's whole-std call summary (`generated::demand`) instantiates std generics the program doesn't reach.

Remaining, all in the issue's Phase 2: error-set seeding of std's function-type sites (about 11 files; I've asked the coordinator first), the instance demand above, union views (2 files), and message differences.
