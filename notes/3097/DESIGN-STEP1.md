# #3097 step 1: a cheaper cross-worker wake — design notes

Measured motivation (step 2 VM run, results-s2.txt): once shards spread, c=1 calls cross workers
and cold-cycle drops 374k -> 49k cycles/s (p50 2.5 -> 20 us): ~10 us per hop on the VM.

Per hop today (sched_wake.velt `unpark`, sched_park.velt `park`):
1. waker: state CAS, inbox pushStack, park word load; if parked: E_UNPARKS inc, CAS, io.wake
   (eventfd write / kevent NOTE_TRIGGER) = 1 syscall.
2. target: epoll_wait/kevent returns (kernel wakeup + context switch = the us), eventfd read
   (2nd syscall), IO_RLOCK, deliver, timers, flushDue, tick.turned, sp.slept, mon.kick.
3. target parks again with nothing to do: `parkable` calls `wk.handOffOutput()` and, on any worker
   but 0, `wk.unpark(g)` -> worker 0 wakes (spurious eventfd write + full wake of worker 0), and
   worker 0's `end.mayPark` scans every worker; after any wake, `mon.kick(g)` (futex wake of the
   monitor thread when it slept).

Candidates (each measured alone, c=1 cross-worker call cost + async rows):
A. Wake worker 0 at a sibling's park only when the program may end: main's root left
   (`t.rootLeft`) or an exit is due / unref'd timers wait. While main's root still runs nothing
   can end, so the wake is spurious. Race: the root may leave just after the sibling looked; the
   side that ends the root already wakes worker 0 if its park word is up (sched_main_end
   `mainDone`), and worker 0 parks only via `end.mayPark`, which rechecks: check the ordering
   (seqcst root-left store vs park word) holds for the sibling skipping the wake.
B. `mon.kick(g)` after a park only when the monitor sleeps for a reason this wake changes (it
   sleeps when every worker parked: a wake by an inbox push means work is coming; the monitor is
   needed for stall rescue only once a poll runs long) — maybe kick lazily from the first poll.
C. A bounded adaptive spin before io.park: watch the park word / inbox for N us (start ~10-20 us
   on the VM, capped, halved after a spin that found nothing, doubled after one that found work,
   0 at T=1 and when the worker spun empty for the last K parks). Coordinator: cap it and show
   idle-server CPU% before/after.
D. Skip the eventfd read when the wake came through the park word (kqueue EVFILT_USER clears
   itself with EV_CLEAR; eventfd needs the read unless EFD_SEMAPHORE... check reactor_epoll).

Gather-back for step 2 (shards spread by a burst never come back): options
 - offer only when the queue delay stayed high for 2 windows (hysteresis);
 - a woken anchored task whose home worker is parked and whose waker's worker is not saturated
   runs on the waker's worker instead (carry at wake: the waker's thread can't touch the home
   block... needs the home to hand over, or the carry to happen at the home's next look);
 - idle workers hand their anchored tasks back to worker 0 / the waker after N idle windows.
