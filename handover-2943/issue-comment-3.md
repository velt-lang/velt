Follow-up (mac-e688): **packages are now in the unit too**, validated and on by default in disk mode. Run it with `VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=<dir>`; `VELT_UNITS_LIB=0` turns the packages off.

Instructions retired, `velt check`, at 66ef82d85 with packages on:

| program | lazy | from disk | |
|---|---|---|---|
| todo_api | 3.76 G | 0.75 G | **5.0×** |
| http_hello | 3.67 G | 0.73 G | **5.0×** |
| wordfreq | 0.60 G | 0.68 G | 0.88× |
| hello | 0.37 G | 0.52 G | 0.72× |

Wall time, interleaved best of 3, comes to 7.6× and 9.3× for the two package programs, but the load average was 76, so lazy's times are inflated.

How it works:
- The dependencies are checked after std's own modules and frozen with it. The key adds each dependency's source tree.
- A disk check's program includes the dependencies' definitions, so the driver's `declare` checks see them.
- Dependency modules without `unsafe` code load as stubs. The others load from source, because the trust check reads their bodies.
- The async closures' analysis walks the dependency bodies; std's still come from its summaries.
- An error in a dependency checks the program again with the dependencies in the app's unit, so the app's errors are reported with it.
- The unit also keeps the JS API memo.

Validation:
- 101 package goldens plus sample300 plus diff54, 450 programs: with packages in the unit, every one gives the same output as std-only disk mode. All 405 that match lazy there still do, and the other 45 differed already.
- Default disk mode vs the wind-down binary: 341 the same, the 8 that changed now give the same output as lazy, none worse.
- In-process units: 214/1/0, as before.
- The `velt_sema` and `veltc --test units` tests pass.

Next is M3, `velt build` from the unit. lazy std's live set is already checked to be a superset of what lowering builds (`Cx::assert_live`, `tests/live_superset.rs`). So the program can be built from the app's definitions plus that live set, with std's bodies decoded as the walk reaches them, and lowering stays unchanged. #2724 and #2766 come in at that point.
