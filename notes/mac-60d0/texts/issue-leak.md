`std_scheduler_rescue a_line_printed_on_a_lazy_worker_comes_before_what_the_rescued_task_prints_next` failed in the merge queue on ubuntu (run 44402225385, #3174's entry, which doesn't touch this path):

```
run 8: blocks left unfreed (rc stats: retain=0 release=0 alloc=1 free=1 blocks=20/19 panics caught=0)
```

## Reproduction

On main (0eb960cec), Linux, arm64, 9 cores: the test's program (`MOVED_OUTPUT`) built once with `VELT_SCHEDULER=std` and run directly, 16 to 18 loops at once, with

```
VELT_THREADS=1 VELT_RESCUE=1 VELT_RC_STATS=1 VELT_RT_DEBUG_ALLOC=1 VELT_RESCUE_STATS=1
```

5 of 26,000 runs end with `blocks=20/19` (`21/20` when the monitor started two spares). The test itself run 50 at once showed nothing in 400 runs, and macOS showed nothing either.

## Cause

`std/lang/hooks/thread_posix.velt` `startThread` takes a counted block (`m.zeroed`, 144 bytes) for the `pthread_attr_t` and the `pthread_t`, and frees it after `pthread_create`. The stall monitor starts a spare worker's thread with it (`sched_monitor.velt` `startSpare`) while the program runs on. When the monitor's thread loses its core between `pthread_create` and the free, the spare runs the rescued task to its end, `main` finishes, and the program takes its block counts before the monitor frees the block. Nothing waits for the monitor's thread: it is not a task.

A probe in the runtime (every counted block with its size and thread, listed at the report and again 100 ms later) named the block: size 144, made on the monitor's thread by `startThread` under `startSpare`, unfreed at the report and freed within 100 ms after it. So no block is lost; the count is taken while the free is still to come.

## Fix

The room comes from the C library, outside the block counts (`m.lastingZeroed`, `m.lastingRelease`), as the executor's other blocks that threads the counts don't wait for use.
