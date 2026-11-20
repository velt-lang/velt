velt-lang/velt#3274 (pull request to follow) changes what users can observe:

A spawned task whose root waits on a promise nobody can settle, and that nothing can wake or join any more, is freed: what its root holds is dropped, and the `[Symbol.dispose]` hooks of those values run, as when the function returns. Before, such a task stayed until the program ended and its values' hooks never ran.

Where the docs describe tasks (`spawn`) and `using`/`[Symbol.dispose]`, they could say:
- a task that can never run again is collected with what it holds, and the dispose hooks of what it held run at that point (Node collects the function too, but runs no hook);
- what is not collected (a root waiting in a combinator such as `Promise.all`, a promise handed to another task and dropped there while suspended, two tasks waiting for each other's join handles).

Source: std/lang/hooks/sched_task_lost.velt (module docs) and tests/golden/lang/task_parked_freed.velt.
