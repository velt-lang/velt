Train failure addressed in eb9c6e769, with main merged in (1dbbbb6ab).

**What changed:** `sched_node_done.test.velt` imports the hook modules by their `velt/lang/hooks/...` path instead of `./sched_*`.

**Why that spelling:** it was the only std test importing scheduler hook modules by relative path. `sched_flags.test.velt` imports the same modules (`sched_node`, `sched_task`) by their `velt/` path and passes on Windows.

**What I could and couldn't check:**
- It doesn't reproduce on macOS arm64: the old head passes in release and debug, with a warm cache and with a fresh `VELT_HOME` as std_tests runs it.
- So I can't confirm the cause on Windows here. A Windows run of `velt test --release std/lang/hooks/sched_node_done.test.velt` at 9755cd194 (should fail) and at eb9c6e769 (should pass) would settle it.

**The compiler side** is #3731:
- the module keying of a relative std import;
- `sched_hooks` skipping a hook it doesn't find without a word, so the miss surfaces as an ICE in lowering.

**Checks at eb9c6e769:**
- the test passes 4/4 in release and debug (fresh `VELT_HOME`);
- precheck passes.
