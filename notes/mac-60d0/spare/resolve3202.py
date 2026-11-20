"""#3202 merged with main: main's E_BLOCKING keeps 992; the spin's words follow it."""
P = "/Users/andii/dev/velt/branches/cross-wake/std/lang/hooks/sched_task.velt"
s = open(P).read()
a = s.index("<<<<<<< HEAD\n")
b = s.index(">>>>>>> 4e8bc7f1fdeada33c74e442383eb93f775d6ea24\n") + len(">>>>>>> 4e8bc7f1fdeada33c74e442383eb93f775d6ea24\n")
new = """/**
 * Worker 0's count of the blocking pool's jobs queued or running (sched_blocking.velt), counted
 * atomically: the program's end after `main` waits for them (sched.velt `schedFinish`).
 */
export const E_BLOCKING: usize = 992;
/** The worker's spin budget before it parks, in nanoseconds (sched_spin.velt). */
export const E_SPIN: usize = 1000;
/**
 * Takes of the inbox in a row that found tasks while the worker had nothing else queued
 * (sched_queue.velt `takeInbox`, sched_gather.velt).
 */
export const E_IDLE_RUN: usize = 1008;
export const E_SIZE: usize = 1016;
"""
s = s[:a] + new + s[b:]
open(P, "w").write(s)
print("resolved")
