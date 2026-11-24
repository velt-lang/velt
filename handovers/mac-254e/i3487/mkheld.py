"""Writes crates/veltc/tests/std_scheduler_held.rs from the #3498 programs."""
import sys

W = "$HOME/dev/velt/branches/fix-3420-254e"
I = "$HOME/dev/velt/branches/r3420-254e/i3487"

src = open(f"{W}/crates/veltc/tests/std_scheduler_waiting_in_hub.rs").read()
start = src.index("use std::path::")
end = src.index("/// A promise of the task (not its root) waits in the hub")
harness = src[start:end].rstrip() + "\n"

HEADER = """//! A task parked for ever while its frames hold a promise that already finished (#3498,
//! std/lang/hooks/sched_node_done.velt): such a promise can never wake the task, but it kept its
//! reference to the task until it was freed, so the park's count had one reference too many and
//! the task, with what it holds, stayed to the end of the program. A finished promise now gives
//! that reference back as it finishes, when nothing but its owner (or its hub) can reach it. Each
//! program runs counted with the checking allocator (`VELT_RC_STATS=1`, `VELT_RT_DEBUG_ALLOC=1`)
//! at 4 and 8 workers: exit 0, its output, and every block freed.

"""

TESTS = [
    (
        "a_task_holding_a_promise_that_finished_unawaited_is_found_parked_for_ever",
        "held_plain",
        "held_plain.velt",
        "parked 20\\n",
        "The root waits for ever while its frame holds a promise that finished without being\n"
        "/// awaited (sched_node_done.velt `letGo`). Before, every run left 280 blocks.",
    ),
    (
        "a_task_holding_a_finished_shared_promise_is_found_parked_for_ever",
        "two_roots",
        "two_roots.velt",
        "parked 20\\n",
        "#3487's `two_roots`: the frame keeps a holder of a shared promise that settled (its hub\n"
        "/// keeps it, and it lets go of its task as it settles), and a promise of the task waits in\n"
        "/// a listed hub. Before, every run left 460 blocks.",
    ),
    (
        "a_finished_promise_handed_to_another_task_is_not_counted",
        "held_handed",
        "held_handed.velt",
        "parked 20, used 20\\n",
        "The finished promise is handed to a task on worker 3 before the root parks, which reads\n"
        "/// and drops it there: the task is found parked for ever, and nothing is given back twice.",
    ),
    (
        "a_finished_promise_kept_in_module_state_is_freed_off_its_task",
        "held_kept",
        "held_kept.velt",
        "parked 20\\n",
        "The finished promise is also kept in module state, which `main` drops off its task at\n"
        "/// the end: its free there gives nothing back to the task, which it let go of as it\n"
        "/// finished.",
    ),
    (
        "a_finished_promise_shared_after_it_let_go_of_its_task_is_read",
        "share_finished",
        "share_finished.velt",
        "parked 40, read 40\\n",
        "A promise that finished, and let go of its task, is shared after: its owner reads it\n"
        "/// through a holder and hands another holder to worker 3, which reads it too. Its hub takes\n"
        "/// it for another task's and settles at the first holder's poll. Before, every run left\n"
        "/// 340 blocks. It guards the let-go itself; the hub's choice of `AWAY` is guarded by the\n"
        "/// one below.",
    ),
    (
        "a_finished_promise_shared_after_it_let_go_with_its_original_handed_over_is_freed",
        "share_orig_handed",
        "share_orig_handed.velt",
        "read 80\\n",
        "A promise that finished, and let go of its task, is shared after on that task, and its\n"
        "/// original holder is handed to worker 3, which reads it after the sharing task read and\n"
        "/// dropped its copy. The hub must take the node for another task's (`AWAY`): taken as\n"
        "/// `HUB_HERE`, the original's drop (sched_share.velt `holderDrop`, the `ORIG_GONE` path)\n"
        "/// would leave the free to a finish still to come, and for a finished node none comes.\n"
        "/// Before, every run left 80 blocks; with `HUB_HERE` for such a node, 40.",
    ),
    (
        "a_finished_promise_shared_into_module_state_is_dropped_off_its_task",
        "share_kept",
        "share_kept.velt",
        "parked 20\\n",
        "As the one above, with the holders kept in module state and dropped one at a time by\n"
        "/// `main`, off the task and under the module lock (sched_share_listed.velt `relist` looks at\n"
        "/// each drop). Before, every run left 320 blocks.",
    ),
    (
        "promises_handed_over_pending_and_awaited_or_dropped_there_as_they_finish",
        "handed_all",
        "handed_all.velt",
        "got 240, dropped 240\\n",
        "Promises handed to other workers while still pending: one awaits each through\n"
        "/// `Promise.all`, another drops others unawaited, each at about the moment it finishes and\n"
        "/// lets go of its task (sched_node_await.velt `awaitWith` and `ownSide`, sched_node.velt\n"
        "/// `pollNode`, which read the task: task 0 is no task to them). A guard: it passes on main\n"
        "/// too.",
    ),
    (
        "a_shared_promise_whose_first_waiter_leaves_after_it_finished_still_wakes_the_next",
        "away_first_leaves",
        "away_first_leaves.velt",
        "got 240\\n",
        "A promise handed to worker 2 and shared there (its hub is `AWAY`): the first holder to\n"
        "/// wait leaves in a race after the promise finished and let go of its task, and a\n"
        "/// `Promise.all` holder waiting behind it still gets the value. A guard: it passes on main\n"
        "/// too.",
    ),
]

out = [HEADER, harness]
for fn, name, file, want, doc in TESTS:
    out.append(f"\n/// {doc}\n#[test]\nfn {fn}() {{\n    check(\"{name}\", {name.upper()}, \"{want}\", 3);\n}}\n")
for fn, name, file, want, doc in TESTS:
    prog = open(f"{I}/{file}").read()
    if '"#' in prog:
        sys.exit(f"{file}: contains a raw-string end")
    out.append(f"\n/// The program of [`{fn}`].\nconst {name.upper()}: &str = r#\"{prog}\"#;\n")
open(f"{W}/crates/veltc/tests/std_scheduler_held.rs", "w").write("".join(out))
print("written")
