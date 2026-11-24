Found in #3583's merge train (run 45366883609, Windows, `std_tests` shard 4).

**What happens:** a std white-box test that imports scheduler hook modules by relative path makes lowering panic on Windows:

```
thread 'velt-compile' panicked at crates\velt_vir\src\lower\mod.rs:240:5:
ICE: std without the hook `std/lang/hooks/sched_all::schedAll`
  …\std\lang\hooks\sched_node_done.test.velt (release): `velt test` failed (exit code: 101)
```

The test (std/lang/hooks/sched_node_done.test.velt at 9755cd194) imported:

```ts
import * as m from "./sched_mem";
import * as nd from "./sched_node";
import * as dn from "./sched_node_done";
import * as t from "./sched_task";
```

**Spellings:**
- On macOS arm64 the same file passes, in release and debug, with a fresh `VELT_HOME`, as std_tests runs it.
- `std/lang/hooks/sched_flags.test.velt` imports the same modules as `velt/lang/hooks/sched_node` etc., and passes on Windows.
- #3583 now uses the `velt/` spelling. Confirming that this alone fixes Windows is pending (a Windows run of both heads is requested).

**Where it comes from, as far as I traced it on macOS:**
- `velt test`'s runner (std/test/plan.velt) awaits a `Promise.all`, so the program lowers `Rt::All` to the `schedAll` hook (velt_vir lower/sched.rs).
- sema's lazy std demands the executor's hooks (lazy_std/mod.rs `sched_hooks`) by name, `found.extend(SCHED_HOOKS.iter().filter_map(|h| cx.prelude_fn(h)))`. A hook that `prelude_fn` doesn't find there is skipped silently.
- `prelude_fn` finds a `::` name through `std_def`, keyed by the module's std path.
- My guess, not checked on Windows: a relative import from a std file (`./sched_node`, backslashes on Windows) keys the module differently from `velt/lang/hooks/sched_node`. Then `std/lang/hooks/sched_all::schedAll` isn't found, nothing reports it, and lowering ICEs later.

**What a fix needs:**
- A relative import inside std and its `velt/` spelling resolve to the same module key on every OS.
- A hook in `SCHED_HOOKS` (and the other hook lists) that the demand doesn't find becomes a diagnostic or an `ICE:` at that point, not a silent skip that surfaces in lowering.
- A regression test: a std test file importing a hook module by relative path, which reaches `Promise.all` through the runner, builds in release on Windows.
