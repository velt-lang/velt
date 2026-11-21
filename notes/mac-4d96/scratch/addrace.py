"""mktest3323.py gains the race program and the item-1 wording.  python3 addrace.py <mktest>"""
import sys

p = sys.argv[1]
s = open(p).read()
for old, new in (
    (
        "/// A promise handed on finishes on the task it was handed to and is freed there, while the task\n"
        "/// that started it is parked for ever: the free gave the promise's reference to its task back on\n"
        "/// worker 2, and nothing polled the task to see that it was now parked for ever (9 blocks a\n"
        "/// round, 180 in all).\n",
        "/// A promise handed on finishes and is awaited and freed on the task it was handed to, while\n"
        "/// the task that started it is parked for ever. Its last reference may go there (the owner's\n"
        "/// drop) or on the worker that woke it (the waker's own reference, released after the wake):\n"
        "/// either way the promise's reference to its task went on a thread that is not the task's, and\n"
        "/// nothing polled the task to see that it was now parked for ever (9 blocks a round, 180 in\n"
        "/// all).\n",
    ),
    (
        "/// Hand-overs racing the starting task's own polls:",
        "/// A promise that finishes after one turn, dropped on worker 2 at about the moment it finishes,\n"
        "/// its task then parked for ever: the drop may read the promise's flags before the finish and\n"
        "/// its count after it (once the task gave its reference back), and must not take a count of 1\n"
        "/// for a promise its task still holds. Before, 1,500 blocks or more were left every run; with\n"
        "/// the hand-over decided at the owner's drop only, 1 run in 8 still leaked under load.\n"
        "#[test]\n"
        "fn a_promise_dropped_as_it_finishes_lets_its_parked_task_go() {\n"
        '    check("race_finish", RACE_FINISH, "done 200\\n", 3);\n'
        "}\n\n"
        "/// Hand-overs racing the starting task's own polls:",
    ),
    (
        '    ("TWO_DROPPERS", "two_promises_dropped_at_once_on_two_workers_free_their_task_once", "t_two"),\n',
        '    ("TWO_DROPPERS", "two_promises_dropped_at_once_on_two_workers_free_their_task_once", "t_two"),\n'
        '    ("RACE_FINISH", "a_promise_dropped_as_it_finishes_lets_its_parked_task_go", "t_race"),\n',
    ),
):
    assert s.count(old) == 1, old[:60]
    s = s.replace(old, new)
open(p, "w").write(s)
