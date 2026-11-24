A follow-up to #3498 / #3583, from its review.

**The gap:** a finished promise lets go of its task only when it is no rejection (sched_node_done.velt `letGo` tests `REJECTING`). `REJECTING` marks a promise that *can* reject, not one that did, so a held promise that can reject but fulfilled never lets go. A task parked for ever while holding one is still not found, and it stays to the end of the program.

**The idea:** test `WATCHED` instead, that is, a rejection actually being watched (sched_node_report.velt). Then:
- a fulfilled promise lets go;
- a rejected one held without a waiter keeps the reference its report needs.

**What it needs:**
- a check that nothing else reads the task of a fulfilled, can-reject node after its finish;
- a test where such a promise fulfils and is held while its task parks for ever;
- the rows that #3583 measured.
