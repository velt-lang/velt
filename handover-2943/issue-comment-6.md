Follow-up (mac-e688), M3: `velt build` from std's unit on disk, debug goldens.

**4234 files: 92 failures in 56 files**, down from 118 in 72. No file that passed before fails now.

Fixed on `units-passes`:
- **d10661ae9:** std's unit qualifies its root module's names. In std's stage the root is module 0, `std/prelude/array`, so Array's methods were stored without their `std/` prefix. The passes that know std methods by name (copies such as `slice`, `map`, `reduce`) missed them under units; for example, `nums.slice()` of a `shared` array counted as the shared array itself.
- **ea7104278:** the app's stage starts with `frozen_reads` off. Std's own shared reads (the blocking pool's state) turned it on for every program checked by units, while lazy std turns it on only when the program uses that code. Read callbacks were then rejected at compile time (7 `rwlock_read_*` files).
- **27615fb2a:** under units, the replaced-holders check walks the app plus the part of std's unit that lazy std's live walk finds, not every definition. `xs.sort()` no longer "may run any function value". The 11 error tests of that check still report their errors.

Tried and dropped: building the program set with lazy std's live walk instead of the reach set. Builds cost the same, and 101 more golden files failed.

Two remaining gaps share one cause: summaries computed over all of std are more conservative than what lazy std computes over the std code a program reaches, or they miss what a second check adds.
1. **Error types (about 11 files: `proxy_throws*`, `sort_comparator_any`, `any_apply_list_throws`).** Std's function-type sites, such as `sort`'s comparator, get the app closures' errors only through a second check's seeded sets, and std's unit is built before those seeds exist. A fix needs std's unit to give its own sites error-set variables that the app's stage solves. That's a design change in `throws/`, which I've asked about before touching.
2. **Async-closure crossing (`record_symbol_accessors`, `stream_pull_generator_uncalled_generic`).** Std's parameter summaries mark an argument as "runs on other threads" because some boundary somewhere in std reaches its type. Lazy std only counts the boundaries the program reaches.
