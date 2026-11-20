## The Windows train failure: found and fixed

Head **afe067115**: b462a3e06 (the fix) on 96d778e1f, then main 4e8bc7f1f (train 14) merged in.

**The failure.** Train 10 failed twice on Windows in `std_scheduler_rescue::a_task_sleeping_on_a_retiring_spare_goes_to_worker_0_with_its_sleep`: worker 0 was not lazy again after the first rescue (`rescues=29-84 spares=4 locks-on=1`). The test alone failed about 2 runs in 15 on Windows, and never on the Mac.

**The cause.** The spin before a park (`sched_park.velt`) returned without the look at a retirement request that both other ways out of a park make (`retiring(e)`). The monitor's request wakes a spare exactly once (`sched_relax.velt` `ask` stores `R_ASKED` and unparks it, and only asks spares that have not answered). When that wake ended a spin, which only takes `PARK_SPIN` down, the spare went back to its loop, found nothing, and parked for real with the request unanswered. Nothing woke it again, it never retired, and worker 0 stayed shared. More spares mean more spinning, which is why the failures came with 4 spares, and Windows spins more.

**The fix.** The spin's way out calls `retiring(e)` as the other two do.

**Verified on Windows** (by the coordinator, at afe067115): the test alone failed 0 of 60 runs (before: 2 of 15, about 13%; 0/60 has p≈0.0002 if the bug were still there). The diagnostic loop (`notes-3202/spare.ps1 -Pipe` on branch `cross-wake-diag`, 500 runs) showed no failure either. All its runs used one spare, as expected once spares retire on time: before, a spare that missed its request stayed up and the next rescue started another.

**No deterministic test.** The window is a spin that starts between the monitor's calm check and its request, which can't be forced without a hook. The rescue test is the one that caught it and now passes.

**The merge with main.** `sched_task.velt`: main's `E_BLOCKING` keeps 992; the spin's `E_SPIN` and `E_IDLE_RUN` move to 1000 and 1008 (`E_SIZE` 1016). Std-lint is clean, and on the Mac the program passes 20/20.
