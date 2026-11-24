Review addressed: ee2ecb521, f6276637c, 5a31f64a2, 5c1fd7a25.

**1. Task 0 in the readers that compare a node's task:**
- `awaitWith` takes the plain path only for a nonzero task (sched_node_await.velt).
- `pollNode`'s adoption test requires `task !== 0` (sched_node.velt), and `nested` refuses task 0 (sched_node_lost.velt).
- `ownSide(n, fl)` takes the caller's flags and counts task 0 as its own side only when `fl` says `FINISHED`. A stale `fl` takes the off-task path, whose atomic or sees `FINISHED` and drops the result once. `droppedAway` gives task 0 to `nd.release`.
- **Tests of the two shapes**, counted guards (they pass on main too; neither window was hit here, as in your runs):
  - `handed_all`: handed promises, still pending, awaited through `Promise.all` on worker 2 and dropped unawaited on worker 3 as they finish;
  - `away_first_leaves`: a promise handed to worker 2 and shared there, whose first waiter leaves in a race after the finish, with a `Promise.all` holder behind it.

**2. Coverage:**
- **The `N_REFS` and `DETACHED` conditions:** a white-box test, `std/lang/hooks/sched_node_done.test.velt`, on a node and task built by hand. With an extra waker reference, a dropped owner, or a rejection, the node keeps its task. Each break fails its own case:
  - without `N_REFS === refs`: "stays while a waker of it is out";
  - without `DETACHED`: "stays when its owner dropped it".

  No std shape reaches a finish with a second waker of the node out: a by-value wake hands its reference to the woken stack, and combinators and adoptions poll with their own cx. I traced the racing program on #3559's pause, and the node finishes only after the wake that runs it. The condition guards future ones (a waker-clone API, say).
- **The counted `ICE:`:** with the counters on, a wake by value would check that the node kept its task while the wake was under way. It lives in #3559's `wakeAway`, so it comes as a small PR once #3559 lands.
- **The share path:** `share_kept` (holders of a finished task-0 node kept in module state, dropped by `main`) frees what main leaks (320 blocks). Taking the hub as `HUB_HERE` instead of `AWAY` is harmless: a finished node held by its hub has a count of 1, so `relist` never lists it. The tests now say which break each one guards.

**Checks on 60b989f58:**
- held 10/10;
- the white-box test 4/4, with blocks balanced. The gate's std-tests shard first caught it leaking its hand-built blocks; that's fixed in 5c1fd7a25, and the shard passes;
- precheck passes. The gate's other failure is #3440.

**Rows:** within ±3% on x86 and arm64.

| Row | x86 T1 | x86 T4 | arm64 T1 | arm64 T4 |
|---|---:|---:|---:|---:|
| `all_small_stored` | +2.37% | +2.35% | +2.21% | +2.36% |
| `fanout_all` | +2.22% | +1.54% | −1.14% | −1.31% |
| `race_small` | +1.18% | +1.15% | +1.07% | +1.14% |

- **`spawn_many` arm64 T4** reads +4.7%, on a row that read −5.0% last time and moves widely at 4 workers there. On x86 it reads −0.55%.
- **Everything else** is within ±0.9%.

**Follow-ups filed:**
- #3595: `WATCHED` instead of `REJECTING`;
- the `away_watch2` hang is #3593.
