Under the executor in Velt, each awaited `yieldNow()` of a task's root now takes a turn of its own (#3687, second of two parts). This applies to the children of an inline `Promise.all` awaited directly in `main`, or in a started promise its owner adopted. They now take their yield turns in the order they yielded, as in Node.

**The problem:** the root's awaited yields in one poll shared one turn ("a second yield's is the first one's"), with readiness given by `T_STATE == YIELDED`. The children of an inline `Promise.all` that the root awaits yield with the root's cx, so whichever child was polled first after the root's turn went on (#3611). A started promise adopted by its owner behaves the same way. After it awaits a settled promise, or after a rejected `Promise.any`, its later yields take the root's path.

**The change** (std; velt_vir for the inline all's cx; one line in velt_sema):
- **The inline `Promise.all` polls its children with a copy of its cx** (velt_vir async_fn/all.rs `child_cx`: the same two words at another address). A yield with the task's own cx (`T_CX`) is the root's own, at most one per poll with no yield block in use, so it keeps #3706's path: one test, token 0, nothing allocated. A yield with a task's waker in another cx is a child's (`sharedRootYield`), and takes the checks that order several yields of one poll. Only the inline all does this: `schedAll` and the race give each child a cx of its own already. rt_abi_async.md's `velt_rt_yield_now` row states the rule.
- **Each root yield in a poll takes a turn of its own.**
  - The poll's first root yield in a task with no yield block in use only marks the root `YIELDED`, and returns token 0.
  - A second root yield in that poll (a child's) makes the block and takes a held record (`rootYieldAgain`): #3706's token, here with the root as its waker. `ROOT_ALONE` gives the first yield its turn first.
  - With a block in use, a root yield behind listed yields takes a record (`rootRecord`), so a yield made later in the same poll comes after it.
- **The token a yield's suspension keeps** (`schedYield`'s result, read by `schedYieldReady` and `schedYieldDrop`): 0 means no record (it goes on once the poll its root yielded in is over, `YIELDED` lasting exactly that poll; or it was made off the workers); anything else is its held record.
- **A token 0 asked by another cx gets a turn of its own** (`pollReady`, `otherTurn`). This covers a child of an inline all that an early rejection detached to run on its own.
  - The token reads the state of its own task: a started promise's own task, else the task polled here.
  - Asked while that task is still `YIELDED`, it isn't ready. It takes a record at the root's placed turn (`B_ROOT`), so it keeps its place among the yields of that poll, with the node flagged `YIELDING` so a second ask adds no second record. Asked on another task's thread, it wakes that waker to ask again.
  - Asked after the root yielded again in a later poll, it goes on one turn late, never early.
- **`fireYields` polls the root at each yield's turn, even when it yielded in this poll** (sched_fire.velt `resume`'s `yieldTurn`). With a record per yield, only the yield whose turn this is goes on. Sleeps' turns keep the holdback.
- **After the turns (sched_run.velt `runWoken`), an empty block is parked and `LOCALS` goes.**
  - `ls.park` moves the block to `T_PARKED` and clears `T_YIELDS`, so the root's later yields take the no-block path again.
  - `ls.block` un-parks the block with its stamps and generation as they were.
  - A turn the root placed in this poll before parking (`ROOT_ALONE`, `B_ROOT`) keeps its place when the block comes back. `runOther` clears the mark at the next poll's start.
  - `leftLocals` clears the task's `LOCALS` flag when no started promise, sleep or yield is left, so its polls take the straight path again.
- **Consecutive turns of the root go on in one poll when nothing could run between them** (sched_yield_list.velt `nextTurn`). A turn that polls the root marks the yield whose turn is next (`turnNext`). A held root record asked in that poll goes on at once, and its own turn only frees it, if:
  - its turn is that next one;
  - this turn's waiter already went on;
  - no microtask, rejection to report or woken promise is pending.

  Two children of an inline all that yielded once each then go on in one poll, as on #3706, in the order Node runs them.
- **While only the root's records are listed, the root's placed turn keeps no record** (`join`, `rootFirst`). The first yield that wakes something else gives it one, by its stamp. Until then the next poll's `fireYields` runs the root's turn first. A yield block keeps up to 8 records for reuse (`recAlloc`, `recFree`, on its task's thread).
- **Stored yields still share the root's turn in a poll**, as before.
- **velt_sema lazy_std `SCHED_HOOKS`** lists `sched_yield_drop` beside the other two yield hooks.

**Still different from Node:** #3689. An inline all's children go past an `await` of a settled promise before their siblings' first steps run (the await-step model).

**Tests:**
- **std/promise_combinators_order_root** (new): promise_combinators_order (#3706) with its combinators awaited directly in `main`, plus sections for:
  - a throw at once beside two and beside three yielders (the yielders go on detached);
  - the root's own yield between a started promise's listed yield and one made later in the same poll;
  - a microtask queued at the first, or the second, child's turn;
  - a child that queues a microtask and yields again at its turn;
  - the first child in the array yielding after the second (a gate the second opens);
  - a started promise's yield between two children's.

  `check: no leaks`; its `.out` is Node 22's.
- **std/promise_combinators_order** gains two last sections with the adopted started promise. One awaits a settled promise before an all; the other runs a rejected `Promise.any` before one.
- **std/lang/hooks/sched_yield_token.test.velt** (new, 8 cases): a token 0 isn't ready in its poll; asked with another waker, that waker gets a turn; asked late, it waits one turn more; asked in its poll, it goes where the root's placed turn is, before later yields; asked twice by one started promise, one turn; a block is parked once its list is empty (in a spawned task's root); a root turn placed before parking comes first once the block comes back; a poll's start drops the last poll's mark.
- **bench/async/root_yields_after_all**, **root_yields** and **inline_all_yields** (new rows, in nightly.sh's async list):
  - 200,000 root yields after an inline `Promise.all` gave the task a block;
  - 200,000 root yields in a task with none;
  - 50,000 inline alls of two children that yield once. Never merging the turns, or making the root's record for every join again, shows only there.

The three goldens (std/catch_panic_yield_siblings among them) pass in debug and release at 1 and 4 workers with every block freed. On #3706's std the root form differs.

**Break checks** (std edited in a copy; each makes the root golden or the token test fail):
- the root form differs: the holdback kept for yield turns; a child's second yield sharing the first's turn; a root yield behind listed yields with no record; the inline all polling its children with its own cx (a compiler build, 22 lines);
- the root form differs and the token test fails: a token 0 asked by another cx always ready; no turn for that ask; that turn put behind the yields made so far instead of at the root's placed turn;
- the token test fails: no `YIELDING` flag for that turn; never parking; un-parking re-stamping a turn placed before parking; a poll's start keeping the last poll's mark;
- the root form differs:
  - a held record merged with work pending (6 lines);
  - merged before this turn's waiter went on (2);
  - a record merged that isn't the next one (2);
  - no first turn for the root when its records are listed with no record of its own (6).

**VIR:** for programs without an inline `Promise.all`, the compiler's output is unchanged (100 sampled goldens: 77 built, all byte-identical; the other 23 are libraries without `main` or expected compile errors).

**Rows** on Linux x86-64: cachegrind instructions, `VELT_THREADS=1`, `VELT_RESCUE=0`, LLVM `--release` programs. Base is #3706's head (4862512c1) and head is this change (ec4258839, the same tree before its rebase onto main). Each side was built with its own compiler and std, and the table gives the median of 3 interleaved runs:

| row | base | head | change |
|---|---:|---:|---:|
| async/root_yields (200,000 root yields, no block) | 21,970,670 | 21,967,065 | −0.02% |
| async/root_yields_after_all | 22,371,053 | 22,372,279 | +0.01% |
| async/race_yield | 226,942,930 | 232,751,165 | +2.56% |
| async/fanout_all | 385,701,389 | 385,700,623 | −0.00% |
| async/all_small_stored | 1,927,241,128 | 1,934,963,241 | +0.40% |
| async/spawn_many | 717,774,897 | 719,771,424 | +0.28% |
| async/hot_loop | 2,472,985,316 | 2,472,924,224 | −0.00% |
| async/inline_all_yields | 13,531,481 | 31,278,091 | **+131.15%** (+177.5 per child yield) |
| 50,000 × `Promise.all([yields(2), yields(2)])` | 29,341,965 | 59,185,339 | **+101.71%** (+149.2 per child yield) |

The inline alls whose children yield cost more by design. #3706 gave all of a poll's child yields one shared turn, an order Node doesn't have. Here each yield after the first takes a record and a turn of its own, in the order Node runs them. The merged turns, the record reuse and the recordless root turn took the second program from +262.4 to +149.2 instructions per child yield. The rest is the record and the list's path that a turn per yield needs. An exception for these two rows is requested in a comment.

On arm64 (macOS, median of 5), the same rows are: root_yields_after_all +1.29%, root_yields +0.23%, race_yield +1.30%, fanout_all −0.03%, spawn_many +0.09%, and the second program +163.1 per child yield.

Docs: velt-lang/velt-lang.github.io#354.

Closes #3611. Closes #3687.
