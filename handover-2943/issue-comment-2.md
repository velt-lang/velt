Follow-up (mac-e688): two more steps, both on `units-passes`.

**b3f87a8e9 (on by default in disk mode).** std's stubs are decoded, and std's tree checked, on a few threads. Under heavy load, hello from disk is at 0.80× lazy and todo_api at 3.66×. Instructions: hello 0.50 G, todo_api 1.34 G.

**6162ffb9e, packages in the unit (D6, opt-in: `VELT_UNITS_LIB=1`).**
- The program's dependencies are checked after std, frozen with it and kept on disk. The key covers each dependency's source tree, and the module table holds the package modules too.
- todo_api and http_hello from disk: 0.78 G instructions vs lazy's 3.7 G, so 4.7× fewer (about 6.6× in wall time, but the Mac was busy).
- **Not sound yet.** A sweep over 101 package goldens plus the usual 349 finds 28 differences from the non-library disk mode.
  - Most only change the message. The crossing-callbacks analysis (`ownership/local_async/callbacks`) no longer sees a path through package bodies, because they left the app's async graph.
  - Two lose their only error (`async_closure_package_wrapper`, `declare_in_package`), and one loses one of two.
- Needed before it can be on:
  1. keep and report the library stage's diagnostics (std's stage assumes std is clean);
  2. local_async summaries of package bodies as precise as walking them;
  3. run the native declare checks of package code.

Per the owner, #2724 and #2766 (lowering, #2707) wait until M3.
