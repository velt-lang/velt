Round 1 is pushed at 57ed94d26 (one commit, rebased onto main). Item by item:

**Required 1: the root yield's cost.**
- The root's own-cx yield is #3706's path again: one test, token 0, nothing allocated. The inline `Promise.all` now polls its children with a copy of its cx (velt_vir async_fn/all.rs `child_cx`; rt_abi_async.md states the rule), so only the children's yields take the checks that order several yields of one poll.
- On x86-64 at ec4258839 (this tree before the rebase; the coordinator's table above): root_yields_after_all +0.01%, the 200,000 root yields with no block −0.02%, race_yield +2.56%. On arm64: +1.29%, +0.23% and +1.30%.
- inline_all_yields stays over the band. The exception request above has the numbers and the cuts made since.
- The coordinator's conditions:
  - (a) is the rt_abi_async.md row;
  - (b) is the x86 table;
  - (c): 100 sampled goldens without an inline `Promise.all` give byte-identical VIR (77 built, the other 23 are libraries or expected compile errors);
  - (d): a compiler built with the children polled with the parent's own cx makes std/promise_combinators_order_root differ by 22 lines, at 1 and 4 workers.

**Required 2: a detached child holding a `POLL` token.**
- `otherTurn` stamps its record with the root's placed turn (`B_ROOT`), your probed fix. The token test's cases are updated to match.
- The root golden gains the throw-at-once sections beside two and three yielders, with Node 22's output.
- Breaking it back to `nextStamp` makes the golden differ and the token test fail.

**Required 3: the `YIELDING` flag.**
- A new token-test case: a started promise's waker asking twice in one poll adds one record.
- With the flag neither checked nor set, two token-test cases fail.

**Follow-ups:**
- Never parking now fails the token test (a spawned task's root parks once its list empties), not only the row. The description no longer gives +3.5%.
- `ls.active` and the `PARKED` bit are gone. A parked block lives in `T_PARKED`, and `T_YIELDS` is 0 meanwhile.
- velt-lang.github.io#354 has the `docs-sync` label.
- The inline `Promise.race` that settles at a yield's turn in the root form is unchanged here, and is #3689's.

**New since the review, from the cost work and a self-review:**
- Consecutive turns of the root go on in one poll when nothing could run between them (`sched_yield_turn.velt` `nextTurn`).
- Yield blocks keep up to 8 records for reuse.
- The root's placed turn keeps no record while only the root's records are listed.
- Four more root-golden sections, all Node 22's output:
  - a microtask at the first and at the second child's turn;
  - a child that queues one and yields again;
  - the first child in the array yielding second;
  - a started promise's yield between two children's.
- A token-test case: a channel send in a child's turn makes its receiver go on before the next turn. The self-review agent found that ordering bug in the first version of the merge.
- Each new check has a break that makes the golden or the token test fail. The description lists them.
- Two new bench rows: bench/async/root_yields and inline_all_yields.

**Gate** (macOS arm64):
- the three goldens in debug and release at 1 and 4 workers, every block freed;
- 12 golden sets: promise_combinators, catch_panic, yield, async_, promise_, spawn, all_, await, top_level, race, sched and timer;
- the WebAssembly promise_/all_/yield/async_ goldens (22/22);
- std hooks (113), task (14) and sync (20) in release at 4 workers; the token (9) and held (3) tests at 1 and 4 workers;
- velt_sema lazy_std and velt_vir unit tests; clippy (velt_vir, velt_sema);
- eager_std_identity, std_lint and precheck.
