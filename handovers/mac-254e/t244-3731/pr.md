Closes #3731.

A std file now gets one module name, however the path that reached it is spelled.

**The cause** (velt_loader `Origin::canonical`, locate.rs):
- A module's canonical name is its path relative to its origin's root, as the two paths are spelled.
- A std test file can be reached by another spelling than the std root: on Windows the verbatim `\\?\C:\…` form a canonical path has (or another case), or anywhere through a link.
- Its relative imports (`./sched_node`) then came out as `std/../std/lang/hooks/sched_node`: a second name for the std module.

**What the second name broke:**
- sema's lazy std demands the executor's hooks by their canonical names (`sched_hooks`: `SCHED_HOOKS` through `prelude_fn` → `std_def`).
- Under the second name it found none, and `filter_map` dropped them without a word.
- The miss surfaced in lowering as `ICE: std without the hook std/lang/hooks/sched_all::schedAll`. The test runner's `Promise.all` needs that hook.
- It broke #3583's and #3706's trains on Windows.

So sema's construct demand didn't miss the hook. The hook was loaded, under a name the lookup doesn't use.

**The fix:** when the plain relative path leaves the root (a leading `..`), `canonical` tries again with both paths' file keys (`paths::file_key`: links resolved, and on Windows the verbatim prefix and the file system's case). The name stays as before when either path still leaves the root, as a user's file outside the root directory does.

## Tests

- **velt_loader `spelling_tests.rs`:** a std test file loaded as an extra root (as `velt test` loads it) imports its siblings by relative path, and every module gets its `std/…` name:
  - plain paths;
  - the std root through a link, and the test file through a link (Unix);
  - the test file by its verbatim path with the std root plain, and the reverse (Windows).

  With the retry removed, the link case fails with `["std/../std/x/deeper", "std/../std/x/inner", "std/../std/x/inner.test"]`.
- **Reproduced before the fix:**
  - On macOS, with the std root or the test file reached through a link, a std test importing hook modules by relative path hit the ICE in release. The `velt/` spelling passed.
  - On Windows, with main's loader, the ICE came only when the std root and the test file were spelled differently (plain and verbatim, either way round).

  With the fix, every spelling passes on both.
- **No std test:** `velt test` spells the std root and the test file alike, so a std test importing by relative path passes without the fix too.
