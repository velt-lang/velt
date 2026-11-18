Follow-up (mac-e688): **M3 started: `velt build` from the unit** works end to end behind `VELT_UNITS_BUILD=1`, together with the disk-unit variables. Pushed at 334dd1467.

How it works:
- A build from disk is made of what lowering may reach (`units::reach`):
  - the roots are the app's and dependencies' definitions, lazy std's roots (the prelude's helpers, the hooks' modules, every hook and the executor's hooks), the module slots and `main`;
  - the closure follows bodies, types, classes (methods, base, subclasses), interfaces and their implementations.
- The set is handed to lowering as its live set, so any miss is an ICE in a debug compiler, never wrong code.
- Every other std definition is a placeholder and is never decoded.

The goldens found a units gap that also affects in-process units. Some std hooks get a dispatch over the program's classes written into their bodies by sema: any-to-primitive, any text, object own keys, constructor names, prototypes. With units, std's stage writes that dispatch before the app's classes exist. The fix:
- record those functions;
- keep their bodies in the unit;
- check them again in the app's stage when the app adds classes.

Measured: a warm rebuild of todo_api after a one-line edit takes **9.5 G → 6.8 G instructions** (sema 443 → 117 ms; lowering unchanged). hello is still worse, 0.45 → 1.4 G: the reach walk, the program build and std's generated instances.

Debug goldens with builds from disk: **4234 files, 137 failures in 96 files**.
- 73 of them fail with in-process units builds too: units-mode gaps. The largest group is program-dependent error types of std glue (proxy traps: "error thrown outside a throwing function").
- 23 only from disk: mostly error tests whose messages differ (as in the check sweep), plus 6 runtime cases to chase.

The lists are in the Mac's `branches/handovers/mac-e688-2943/val-4/`.

Next: those groups, then M4 (a prebuilt std unit). Lazy std stays the default until units passes the gate (per the owner).
