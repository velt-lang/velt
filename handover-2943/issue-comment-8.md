Follow-up (mac-e688): **65 failures in 35 files**, down from 85 in 50. No file that passed before fails now.

5ed9c07f8: under units, the async-closure analysis (`ownership::local_async`) walks the bodies of the std functions the program reaches, the way lazy std does, instead of std's whole-std summaries. It no longer uses those summaries' parameter facts, generic crossing values or crossing closures. That fixed 15 files:
- 9 error tests whose reason now matches lazy std's;
- `record_symbol_accessors` and the two stream files, which were rejected for a boundary somewhere in std the program never reaches;
- `record_getter_http_client*`.

All 43 `errors/async_closure*` tests report the same first error as lazy std. It costs warm `velt check` instructions: hello 0.54 → 0.71 G (lazy 0.36), todo_api 0.77 → 1.05 G (lazy 3.64). A follow-up can run the walk only when the summary pass makes an app closure non-local for a library reason. `VELT_UNITS_WALK_STD=0` turns it off.

What's left:
- **Error-set seeding of std's function-type sites:** `proxy_throws*`, `sort_comparator_any`, `any_apply_list_throws`, `db_rows_source_throws`, probably `generic_fn_*`. That's about half the remaining failures, and it needs the design change described earlier.
- `errors/http_server/*` message differences (7);
- union views (2);
- `any_function_properties_*` (2);
- a few single files.
