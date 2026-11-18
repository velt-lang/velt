## Status (mac-e688, 2026-11-16): a check from std's unit on disk is about 3× faster than lazy std for package programs

On `units-passes` (66f2985f7 and its test fix, pushed). Run it with `VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=<dir>`.

`velt check`, dev build, lazy std vs std's unit from disk. Interleaved, best of 3 for wall time, median for instructions; at da9cd5dc7:

| program | lazy | from disk | |
|---|---|---|---|
| todo_api | 283 ms, 3.79 G | 92 ms, 1.42 G | **3.07×** |
| http_hello | 277 ms, 3.70 G | 88 ms, 1.35 G | **3.15×** |
| wordfreq | 41 ms, 0.61 G | 41 ms, 0.69 G | 1.00× |
| hello | 22 ms, 0.35 G | 41 ms, 0.68 G | 0.53× |

At the wind-down (2d4dfbd83) these were 1.22×, 1.41×, 0.24× and 0.11×. At 66f2985f7 hello is down to 0.50 G (about 31 ms) and todo_api to 1.35 G.

What changed:
1. **The load reads nothing of std.**
   - A snapshot of std's directories and files, kept beside the unit and checked with one `stat` each, answers the loader's questions about std's paths.
   - std's module table (path, file, imports) is kept too, and its modules become the load's first modules, with their stubs.
2. **std's definition tables decode on demand.**
   - The unit's entries are length-indexed, and its AST fragments sit in a numbered table.
   - A covariant slot vector is filled on first read.
   - The app's check decodes about 2,000 of std's 22,416 entries; the eager part of the decode takes about 5 ms.
3. **Per-unit summaries replace the whole-std scans** the app's stage still made:
   - the calls and vtable slots behind the generated demand;
   - the instantiation sites;
   - the names and types outside bodies (local_async);
   - the module `let`s and the std type count;
   - the value-params scan (the fixpoint over std's scan plus the app's is solved again);
   - the virtual slots, the implicit-calls scan and the host functions.

   `replaced_holders` runs only when the app has a candidate.
4. **A check from disk builds its program without std's definitions.**
   - It keeps the app's, the std interfaces the app implements and std's module slots.
   - std's other entries are placeholders, never decoded.
   - The generated demand starts from the app's concrete calls, as with lazy std.

Validation (349 programs: sample300 + diff54):
- Disk vs the wind-down binary's disk mode: 341 the same. The 8 that changed now give the same output as lazy, so none got worse.
- Disk vs lazy, full diagnostics text: 309 the same (301 before). The other 40 differ before too; 37 of them are in diff54.
- In-process units vs lazy: 214/1/0 and 5/16/33/0, as before.
- The `velt_sema`, `velt_loader` and `veltc --test units` tests and the debug goldens pass.

Fixed along the way:
- A check with syntax errors type-checked std's stubs: thousands of bogus std errors.
- An ICE when std module slots were missing from the program.
- Stubs dropped the `declare function`s that the raw-native-API check needs.

Next:
- hello's remaining fixed cost: the summaries' eager decode (about 1 MB), decoding the 830 stubs, and validating the tree (1,000 `stat`s).
- Then M3: a `velt build` from the unit, with std's definitions decoded as lowering reaches them.
