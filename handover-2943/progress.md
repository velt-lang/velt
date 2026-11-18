# #2943 disk unit: mac-e688's progress (from 2026-11-16, after mac-161d's handover)

Branch `units-passes`, worktree `branches/d1-open-pr`. Scripts here have `S=` set to this folder.
`one.sh <lazy|disk|units> <prog>` prints one run's stage times and instructions;
`diskchk.sh <file>` compares a disk-unit check with lazy (exit code + sorted diagnostics);
`tools/prof.py`, `tools/callers.py`, `tools/selfunder.py`, `tools/phases.py` read samply profiles
(`samply record --save-only --unstable-presymbolicate -o x.json velt check ...`).

## What changed (commits on units-passes after 2d4dfbd83)
1. a8ce632bc: std's file tree snapshot (`velt_loader::std_tree`, kept beside the unit as
   `<hash>.tree`, checked with one stat per dir/file). The loader asks it for file_key, is_file,
   dir names; a load with stubs reads no std source (filled for messages by
   `units_disk::fill_sources`). with_all_std lists std from it. IoTypes::find canonicalizes only
   matching names.
2. std_tree answers from precomputed maps; disk arena and checked program leaked at exit.
3. fd0cc56bd: **lazy decode**: `DefTable` entries decode on first use (`ctx/slots.rs`, a covariant
   slot vector; `ctx/def_table.rs` `Lazy`). Codec: AST fragments in a numbered table (`Frags`),
   tables written as length + entries + end offsets (FORMAT 3). Summaries added so the app's
   stage reads no std entry: calls + vtable slots (generated::demand), instantiation sites,
   outside-body names/types (local_async), module lets, std type count (struct_tuples), value
   params scan (exclusive), virtual slots (locked_props), implicit scan (replaced_holders),
   host fns. replaced_holders runs only when the app has a candidate.
4. 4adc65d3d: with std from disk (`Ctx::std_from_disk`, `velt check` only), the program is
   built from the app's definitions (+ std ifaces the app implements + std module slots);
   std entries are placeholders (`program::placeholder_def`), never decoded.

5. da9cd5dc7: std's module table kept beside the unit (`<key>.modules`: path, file, imports,
   jsx runtime per std module); a load with stubs injects them as its first modules
   (`LoadOptions::std_modules`, `Loader::inject_std`), resolving nothing of std. Sweep fixes:
   the recovery path reloads real std; std module slots are built in the program; stubs keep
   `declare function`s and unsafe functions (native trust check).
6. 66f2985f7: from disk, the generated demand starts from the app's concrete calls only
   (std's own instances are its unit's); leftover tables forgotten; check.rs file keys from the
   snapshot.

## Numbers (`velt check`, dev build; interleaved best of 3 at da9cd5dc7)
| program | lazy | disk at 2d4dfbd83 | disk at da9cd5dc7 |
|---|---|---|---|
| hello | 21.7 ms 0.35 G | 302 ms 2.6 G | 41 ms 0.68 G (0.53x) |
| wordfreq | 41 ms 0.61 G | 324 ms 3.0 G | 41 ms 0.69 G (1.00x) |
| http_hello | 277 ms 3.70 G | 431 ms 3.9 G | 88 ms 1.35 G (3.15x) |
| todo_api | 283 ms 3.79 G | 479 ms 3.9 G | 92 ms 1.42 G (3.07x) |
At 66f2985f7: hello 0.50 G (~31 ms quiet), todo_api 1.35 G.

## Validation (at da9cd5dc7, `basechk.sh` over sample300 + diff54 = 349)
New disk vs baseline disk (2d4dfbd83, `branches/e688-base`): 341 the same, 8 changed, all 8 now
the same as lazy. New disk vs lazy: 309 the same (baseline: 301); the other 40 differ in the
baseline too (37 are diff54's known units differences).

## State at b3f87a8e9 (pushed; status comment on #2943: issuecomment-6921245199)
Validated at 66f2985f7 (+ test fix d0cb182c9): disk sweep vs baseline: 0 regressions (341 same,
8 fixed); units sweep 214/1/0 and 5/16/33/0 as before; velt_sema 108, velt_loader 139, veltc units
13 tests, debug goldens: pass. b3f87a8e9 adds parallel stub decode + parallel tree stat + a global
touch switch (perf only; re-run the sweep before relying on it).

hello's remaining fixed cost (~0.50 G): loader/stub decode ~20%, eager decode (summaries 1 MB)
~18%, entry decode ~7%, drops ~6%, tree check ~5%, weak_threads/locked/ownership tail.

## Next: packages as units (D6), the biggest remaining win for package programs
todo_api is 94 lines; ~7.5k of its lines are the `http-server` package, which every check
parses and checks again. Plan:
1. A "library" stage = std + dependency modules (`is_std || is_dependency`) in `units::Order`,
   stubs, disk save/load (14 `is_std` sites in units code).
2. `stubs::merge` / `std_found` assume std has no namespaces, global props, brands, promoted
   consts, nested closures: take those from the library run too.
3. Key: std tree + each dependency's files (path, len, mtime) + lockfile; load every package
   module as an extra root (like with_all_std) so the unit does not depend on the app.
4. Loader: package modules from the module table + stubs too (skip parsing them).
5. Package diagnostics (warnings) replayed from the unit (`diags` is saved).
Then M3 (velt build from the unit: program from reached defs, decoded on demand).

## Owner decision (2026-11-16)
#2724 (lowering pass dump) and #2766 (Map family shared by shape, debug) are #2707 lowering
work: not needed for the check work; wait for them until M3. #2766 touches
`body/lazy_std/mod.rs` (expect a small conflict with stub_unreached) and adds a std file.

## D6 result (6162ffb9e, `VELT_UNITS_LIB=1`, off by default)
Dependencies in std's unit: todo_api and http_hello from disk 0.78 G vs lazy 3.7 G (4.7x fewer
instructions; ~0.77 G either). Sweep `libchk.sh` over pkgs.txt (101 goldens in packages with
dependencies) + sample300 + diff54 (val-3/lib.txt): 28 differ from the non-library disk mode
where that one equals lazy. All in package interactions; most only change the message (the
crossing-callbacks analysis, `ownership/local_async/callbacks`, sees no path through package
bodies, which left the app's async graph). Not sound yet: 2 lose their only error
(`async_closure_package_wrapper`, `declare_in_package`), 1 loses one of two. Needed:
1. the library stage's diagnostics kept and reported (an error in package code; std's stage
   is assumed clean);
2. local_async summaries of package bodies as precise as walking them (the reason a callback
   crosses through a package's code);
3. the native declare checks of package code.

### Why D6 misses errors, and the design to do it right
The std stage is treated as trusted: passes skip checks there (std is clean), and `rounds::run`
would return an error found there, so the missed errors are never detected, not dropped. Putting
packages in `Stage::Std` (what 6162ffb9e does via `units::in_first_unit`) is the wrong shape.

Right shape: a chain of units, each checked as an app on top of the frozen one below it:
1. std's stage as today (std modules only: `in_first_unit` back to `is_std` for it);
2. a **library stage**: the existing App-stage machinery (`rounds::run` with `Stage::App`)
   over the dependency modules on top of std's unit, with no root/main; its errors return as
   any app's do, its warnings stay in `cx.diags`; then `units::finish_unit` freezes it (the
   summaries are computed from 0, so they cover std + packages; on disk this happens once);
3. the app stage on top of the library unit (`Order` with std+packages as the base).
In `check_timed` (velt_sema/src/lib.rs): `new_cx` captures `order`; it needs the order of the
current stage (a `Cell`/`RefCell` switched between the library and the app stage), the rewrite
facts installed per stage (`found.install`, `remap_module_facts` with that stage's `in_app`), and
the generated instances bound in the stage that demands them. The disk unit saved is the
library unit (key: std tree + dependency trees, as now). The loader side of 6162ffb9e (module
table with package modules, `StdModule::package`) stays as is.

## D6 after the fixes (fef6a3d86, bfc83591d, 66ef82d85)
- Program of a disk check includes the dependencies' definitions (`unit_sums.lib_base`,
  `Ctx::program_from`), so the driver's declare checks see them.
- Dependency modules with `unsafe` code load from source (trust check reads bodies); the rest
  as stubs from the module table.
- The async analysis walks dependency bodies (`Ctx::walked_from`); std's come from summaries.
- An error in the dependencies' stage turns libraries off and checks again with them in the
  app's unit (`units::libraries_off`), so the app's errors are reported too.
- The unit keeps the JS API memo (`js_api_modules`).
todo_api with `VELT_UNITS_LIB=1`: 0.75 G (lazy 3.8 G). The 5 package programs that differed
(declare_in_package, async_closure_package_wrapper(+_promise), package_runs_callback,
runtime_declare) and 12 of the message diffs checked now equal lazy. Full sweep pending.

## M3 plan (velt build from the unit)
lazy std's live set (`body/lazy_std/live.rs`) is checked to be a superset of what lowering
builds (`Cx::assert_live`, velt_vir `tests/live_superset.rs`). So: build the program from the
app's definitions + the live set walked from `main` (std bodies decoded as the walk reaches
them), placeholders for the rest; the program-build passes over that set (not "from the app
on"); lowering unchanged. Then measure `velt build` debug/release (`e2e-build.py`). #2724 and
#2766 (lowering, #2707) come in then (owner).

## M3 step 1 (1d811df76, `VELT_UNITS_BUILD=1` with the disk-unit variables)
`velt build`/`run` check by units with std from the disk unit; `CheckOptions::check_only`
(set by `velt check`'s path, `driver/recover.rs`) keeps the placeholders and the narrowed
generated demand to checks, so a build's program has all of std (decoded on demand, all of it
for now). The generator host's build stays lazy. Warm rebuild after a one-line edit (debug,
instructions): todo_api lazy 9.6 G -> 6.8 G (1.41x); hello 0.47 G -> 1.53 G (all of std in the
program). `buildab.sh <dir> <prog>` measures it (bash: zsh does not split `$E`).
Next, step 2: the program from what lowering reaches. lazy std's live machinery
(`body/lazy_std/live.rs`) is bound to lazy checking (hooks demanded by constructs, gated
roots); a conservative reachability instead must fail loudly on a miss: in a build, a
placeholder should be an extern that does not link (not an empty function), and hook
lookups ICE already. Validate with the goldens run with the units-build variables.

## Owner decision (2026-11-16 evening): replace lazy std, don't delete it now
Order: M3 (builds from the unit via the live set) -> M4 (prebuilt std unit, no cold full-std
check) -> units on by default + full gate on 3 OSes -> then delete lazy std (std_load,
load_more, deferred bodies); the live-set walk moves to the units side.

## M3 step 2 (1b052a491 .. 2ac09ade6): builds from the unit
- `units::reach::program_set`: the program of a build from disk = the app's (and dependencies')
  definitions + lazy std's roots (prelude helpers, hooks' modules whole, every hook, the
  executor's hooks) closed over bodies, types, classes (methods, statics, ctor, vtable, base,
  fields, subclasses), enums, interfaces and implementations; the set is lowering's live set
  (`Program::live`), so a miss is an ICE in a debug compiler. Type definitions the final type
  table names are built as types (`finalize::build_named_types`). Program-build passes ask
  `Ctx::in_program` before reading an entry.
- Found by the goldens: std bodies with a dispatch over the program's classes (any-to-primitive,
  any text, object own keys, ctor names, prototypes: `object_own::dynamic_class_match_where`)
  were checked in std's stage without the app's classes -> silent wrong output in builds by
  units (in-process too). Now recorded (`Ctx::program_dispatch` -> `Summaries::dispatch_fns`),
  their bodies kept whole in the unit (`ast_refs::keep_bodies`), and checked again in the app's
  stage (`units::check_dispatch_again`).
- `any_` goldens with builds from disk: 239 files, 20 failures -> 4 files (8 failures), all the
  same in-process: any_apply_list_throws (proxy traps' throws: program-dependent error types of
  std's proxy glue), any_function_properties_life/_mutex (sema rejection, a known units
  difference), any_to_union_object_member_panics. Same family as diff54's units differences.
- todo_api warm rebuild: lazy 9.6 G -> 6.8 G instructions.

## Goldens with builds by units (334dd1467, debug)
From disk (`VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=...`): 4234 files,
137 failures in 96 files (val-4/gold_disk.txt, gold_fail_files.txt). Of those, 73 fail with
in-process units builds too (val-4/gold_inproc.txt): units-mode gaps (biggest group: "error
thrown outside a throwing function" = program-dependent error types of std glue, e.g. proxy
traps; index-out-of-bounds panics; sharing/clone panics). 23 only from disk
(val-4/gold_disk_only.txt): mostly error tests whose messages differ (async_closure_*,
http_handler_*, as in the check sweep) plus function_call_this, interface_upcast_fn_fields,
literal_accessor_contexts, narrow_field_union_late, error_name_getter_uncaught,
any_to_union_object_member_panics.
- bdb76d0fb: definitions the program's build synthesizes (object-literal accessors / fn
  fields) are in the program and the live set (`Ctx::program_new_from`): fixed
  interface_upcast_fn_fields, literal_accessor_contexts, narrow_field_union_late,
  function_call_this.
- ad8f31b40: a build that reaches `catchUnwind` demands std's panic path
  (`lazy_std::note_catch_unwind`): fixed catch_panic_in_clone, catch_panic_with,
  shared_fn_panic_caught (catch_panic_after_timer still fails: panics after an await).
- Open: proxy_throws* (8): `Proxy`'s methods' error types come from the program's handler
  traps (`body/expr/proxy.rs` any_op), inferred in std's stage without them. Needs the
  methods' bodies kept and checked again (like `check_dispatch_again`, for methods).

## M4 sketch (prebuilt std unit), for after M3
- Where: the build cache (`~/.velt/build-cache/v5/units/`) instead of `VELT_UNITS_DISK`; the
  key is already the compiler identity + std's tree (+ dependencies' trees).
- When: made by the first check/build that misses (as now, ~15 G once), or ahead of time:
  `velt std prepare` run by the installer / a release step, so no user ever pays the cold
  check. A release could ship the unit next to std (key = that compiler + that std).
- Size: ~10 MB unit + 0.6 MB stubs + 50 KB tree + module table per (compiler, std, deps).
  GC with the build cache's existing policy.
- Concurrency: units are written atomically (temp + rename); two processes making the same
  unit both write it, the last rename wins (fine). Concurrent apps with different dependency
  sets get different keys.
- Then: units on by default (`velt check` and builds), run the full gate on 3 OSes; then
  delete lazy std (owner's order).

## Goldens, builds from disk, at ad8f31b40 (val-5/)
4234 files, 118 failures in 72 files (was 137 in 96 at 334dd1467); no new failing file.
Remaining groups: proxy_throws* (program-dependent error-set variables of std's Proxy glue),
error tests whose messages differ (as in the check sweep), sharing/transfer panics in
shared_fn_*/http_server request-after-handler, catch_panic_after_timer, a few sema
rejections already in the units check differences (any_function_properties_*).
- Lead for the caught-panic-after-await group (catch_panic_after_timer, http_request_*,
  shared_fn_panic_caught_*): a build by units demands every hook (laziness off,
  `lazy_std::live::demanded`), lazy only those its live code's constructs demand. The VIRs of
  catch_panic_after_timer differ only in sched hooks (`sched_signal::onSignal` built only by
  units; sched_end/micro/queue/io bodies differ). Next: have `units::reach` collect the
  constructs as `live.rs` `walk_expr` does (`Walk::kinds`, `tasks`, `formats`, ...) and give
  lowering `HookSet::demanded_by(demanded_for(present))` as lazy does.
- 942ab96dd: a build by units runs lazy std's live walk over the reach set
  (`lazy_std::walk_units_set`, `Live::units`): hooks demanded by the constructs, as lazy std;
  what the walk adds joins the program. Fixed catch_panic_after_timer (and the rest of the
  caught-panic group, to confirm with the goldens). Cost: todo_api warm rebuild 6.84 -> 7.68 G
  (lazy 9.5 G), hello 1.42 -> 1.61 G (lazy 0.45 G). Later: seed the live walk properly and drop
  `units::reach`'s own walk (one walk, lazy std's rules).
- 942ab96dd reverted (f96856924): with the live walk the goldens were identical (val-6: same 72
  files, 118 failures) and builds slower; the caught-panic files had already passed after
  ad8f31b40 (a local run that looked fixed was an earlier test-loop artifact). Keep the idea
  for when the live walk replaces `units::reach` (one walk).
- Open (check and build): rwlock_read_call_result_* — units rejects at compile time ("the
  function passed to `read` modifies the locked value") what lazy leaves to the run-time stop:
  std's lock summaries (`ownership::locked`, `Summaries::lock_summaries`) make `Array.at`'s
  result alias the receiver where lazy's analysis of the same call does not. Needs the two
  computations compared for `at`.
- M4 step 1 (this commit): `VELT_UNITS_DISK=auto` -> `<build cache>/units` (resolved in
  `veltc/src/main.rs` before threads start). Not yet under the build cache's size limit / GC.

## Live walk instead of units::reach (VELT_UNITS_LIVE_WALK=1), 2026-11-17
- 10982d078 + 01bb4547a: a build from std's unit runs lazy std's own loop (`lazy_std::reach`:
  walk, hooks demanded by constructs, sched/record/number-text hooks, C exports), seeded from
  module scopes (`live_roots::seed_units`), with the live classes/interfaces in the set (else
  readonly erasure misses their field types: unreduced ErrSite ops in lowering).
- Sets: hello 2809 defs (reach 6373), todo_api 4808.
- Cost: SAME as reach (todo_api warm 7.36 G vs reach 7.43 G, lazy 9.53 G; hello 1.29 vs 1.39,
  lazy 0.42). The 4.32 G in 10982d078's message was from builds that FAILED (sema error before
  codegen) — always check the build's exit/output in A/B scripts (walkab.sh doesn't!).
- todo_api's cost after sema is lowering + codegen (~same as lazy). hello's gap: sema 80 ms vs
  22 (program build 32, round first 31, reach 24, generated 14), parse 54 vs 27.
- Next: goldens with the walk (it demands hooks by lazy's rules: may fix VIR diffs); if equal
  or better, drop units::reach.
- val-7 (live walk, 01bb4547a): 241 failures in 173 files vs reach's 118 in 72 (val-6); 101
  new failing files (layouts, intersections, recursive aliases, ctor values, generators,
  http handler_ctx_*), none fixed. Few are ICEs; most are output/diagnostic differences: the
  passes that build the program (layouts, anon shapes, type ops) read definitions the walk's
  set leaves out. DECISION: keep units::reach as the build set; the walk stays opt-in
  (VELT_UNITS_LIVE_WALK) and is not pursued — same cost, less correct.
- d10661ae9: in std's stage cx.root = module 0 = std/prelude/array, so `Ctx::qualify` and
  `collect/extensions.rs` module_prefix left Array's method names bare (`T[].slice`). Name-based
  std checks (ownership/derived.rs copies/builds, evidence.rs reduce) missed them: fixed
  shared_reads (nums.slice() taken as the shared array). Also affects in-process units.
  rwlock_read_call_result_pop_stops / rwlock_read_closures still differ (closure param marked
  mutable through `s.items.at(1)!` / `cb(s.items[0])`): to investigate.
- ea7104278: app stage starts with `frozen_reads = false` (std/lang/hooks/sched_blocking's own
  shared reads turned it on for every program under units; lazy only when the program uses the
  blocking pool). Fixed rwlock_read_closures, rwlock_read_call_result_pop_stops (all modes).
  OPEN QUESTION for review: a lazy program that uses the blocking pool has frozen_reads on (std
  stage's tracking); under units only the app's reads turn it on. Std's own summaries were
  computed with it on, so std calls are tracked; app functions aren't unless the app reads a
  shared. Believed equivalent (sched_blocking's reads are of its own state), not proven.
- Pitfall again: `env $E` in zsh doesn't split -> a "units" run that was actually something
  else. Use tools/chkmodes.sh (bash arrays) for lazy/inproc/disk comparisons.
- Throws group (proxy_throws*, sort_comparator_any, any_apply_list_throws,
  db_rows_source_throws; lowering ICE "error thrown outside a throwing function"): NOT a
  re-inference gap. Std's function-type sites (sort's comparator) never get error-set
  variables; under lazy std an app closure's errors reach std through a SECOND CHECK whose
  seeds (`FnTypeSeeds::sites`) give std's site the set the first found, so sort's param type is
  interned throwing. Std's unit is built before any seed (and without proxies: no trap_vars).
  Tried: re-inferring std fns with Indirect srcs in the app stage — std has 0 Indirect srcs
  (8441 fns, 18828 srcs); reverted. Fix needs a design: std's unit giving its own sites
  variables that the app's stage solves (readonly::erase already runs at program build), or
  re-checking the std functions whose sites got seeds. Overlaps #379 B (mac-adfe, method/field
  error-set sites): ask the coordinator before touching throws/fn_types.rs.
- 27615fb2a: replaced_holders::check under units walks the app + `lazy_std::units_live_set`
  (lazy's live walk over std's unit, lazy state restored) instead of all definitions: fixed
  assign_target_optional_callbacks, assign_target_replaced_holder_fix; the 11
  errors/assign_target_replaced_holder* still fail in every mode. (The reach set did NOT fix
  it: a superset keeps std callers of sort that pass more than a fn/null/own param.)
- PATTERN: whole-std summaries are more conservative than lazy's live subset. Same class:
  local_async (record_symbol_accessors: `Object.defineProperty`'s param summary CROSSES because
  some boundary somewhere in std reaches its type; stream_pull_generator_uncalled_generic:
  ReadableStreamDefaultController "settles a promise ... another task may await"). Fix would be
  the app stage walking the live std bodies (units_live_set) for those params instead of std's
  AsyncSummaries flags — larger change in ownership/local_async (graph, summaries, call_sites).
- rwlock group (6 more files) confirmed fixed by ea7104278.
- val-8 (27615fb2a, builds from disk): 4234 files, 92 failures in 56 files (val-6: 118 in 72);
  16 files fixed, no new failing file. Left: throws seeding (~11, needs design, asked
  coordinator), local_async whole-std summaries (several), message diffs, misc.
- The issue body's Phase 2 (D1 dispatch contracts, D2 program-wide flags, local_async over
  library closures, ProxyHandler throws sites) IS this gap list; frame fixes as those items.
- 8f4e92d70 (D1 vtable slots): std's unit gives Error (+ subclasses) methods/accessors and any
  std class's toString/toJSON a slot: fixed error_name_getter, error_text_values,
  error_tostring_override. Run-time cost to measure before units default (D1: within 3%).
- 6d592a039 (D2 any members): AnyDefs::members recorded per std fn (dyn_member_fns), app stage
  starts without, program build turns on if app noted or units_live_set holds one. Sound.
  iface_any_args_write_check still differs: live walk reaches std/lang/proxy/array (class
  methods live once `any` exists) -> arrayMethodApply; lazy std doesn't LOAD that module
  without proxies. Units live set lacks lazy's module-demand filter (demand_std).
- units_live_set excludes hooks still waiting (Live::waiting) unless record hooks needed.
- union_view_assigned / union_view_live: fail with VELT_UNITS=1 and =2 (any units mode), pass
  lazy and VELT_EAGER_STD — so not "all std checked" but units machinery. Lazy runs 2 "grown"
  rounds, units 1 (the second view family is never made). Tried: a clean UnionViews per app
  round (by_def/keys/made from std's unit dropped) — no change, reverted. Next idea: compare
  `made`/`assumed` per round (print in rounds.rs) and the FamilyKey (uses `i.module` index).
  DONE that: lazy round 2 makes a second family keyed by the INSTANTIATED intersection ifaces
  (`Base<f64> & { … }|…|Idle|Pending|Ready`), which triggers round 3. Units round 2 makes only
  the generic family (`Base<T> & …`) again -> no round 3. So under units the view made for
  `State<number>` names its members by the generic intersection interfaces: look at how
  intersection interfaces are instantiated/named (intersection_class_parts, anon twins)
  when std's unit is in front.
- 76c6fc310: velt check skips units_dyn_members (program::build 11% -> 2% of a warm hello check).
- val-9 (76c6fc310): 139 failures in 95 files: 43 NEW (http_server/*, datetime, duration) from
  8f4e92d70's open slots, 4 fixed (error_*). Cause: (1) toString/toJSON slots on Date/Duration,
  whose vtables lowering doesn't build; (2) with deps in the first unit, Error re-laid out as a
  dependency class's base (check_classes base_first) without the open slots -> 0-slot vtable,
  virtual calls ICE (dispatch.rs:128). Fixed 1a8efb18b (Error family only; slots over the whole
  layout order). Spot-checked; full goldens queued.
- val-10 (1a8efb18b): 88 failures in 52 files (val-8: 92/56); no new failing file; fixed the
  4 error_* files.
- c621d3fda (opt-in VELT_UNITS_WALK_STD=1): local_async walks units_live_set's std bodies
  instead of their summaries (Ctx::walk_std / Ctx::walks), drops the other std params' facts
  and generic hits. Fixes record_symbol_accessors. NOT yet golden-tested; cost: a live walk +
  walking ~3k std bodies per check (gives back part of units' check speed) -> plan: run it only
  when the summary-based pass reports an error with a library reason (recheck-on-doubt).
- KEY FINDING: units_live_set is a superset of lazy's live set because lazy std LOADS only part
  of std (109-190 of ~830 modules in two goldens; not collections/set, not proxy/array) and
  seeds by-name helpers / opens methods only of loaded modules. Blocks: iface_any_args_write_
  check (proxy/array's arrayMethodApply), streams_any_reason and
  stream_pull_generator_uncalled_generic (collections/set's anySet_forEach makes a dynamic
  value). Fix = emulate lazy's module loading in the units walk (seed/open only modules the
  program's code touches + those hooks demand). Careful: this set feeds soundness-relevant
  decisions (any member slots), so missing a module must fail safe.
- 3c847b900: units_live_set emulates lazy's module loading (start: app modules + their direct
  std imports + std/prelude/*; by-name fns AND construct-free hooks seeded only from loaded
  modules; a module becomes loaded when live code reaches into it, fixpoint). val-11: 85
  failures in 50 files (val-10 88/52), no new; fixed iface_any_args_write_check and
  any_to_union_object_member_panics (both: any's shape checks back).
- Stream files (streams_any_reason, stream_pull_generator_uncalled_generic) with
  VELT_UNITS_WALK_STD=1 still fail: the dynamic conversion at collections/set.velt:159 comes from
  a walked def >= unit_base, likely an app-stage INSTANCE of std's anySet_forEach demanded by
  std's whole-std `calls` summary (generated::demand). Next: filter that demand by the live set.
  WRONG guess: it was the `crossing` summary (crossing_closure_params) carrying std-stage Whys.
- 5ed9c07f8: walk_std ON by default under units (VELT_UNITS_WALK_STD=0 = summaries); crossing
  summary dropped too. Cost warm check: hello 0.54 -> 0.71 G (lazy 0.36), todo_api 0.77 ->
  1.05 G (lazy 3.64). val-12: 65 failures in 35 files (val-11 85/50), 15 fixed (9 errors/
  async_closure_* / sync_closure messages, record_symbol_accessors, the stream files,
  record_getter_http_client*), no new. Optimization owed: run the walk only when the summary
  pass makes an app closure non-local for a library reason.
- Throws gap, coordinator OK'd a prototype on units-passes (no PR; tell mac-adfe, rebase onto
  #3111 after it merges; mac-adfe told). FIRST FINDINGS (they correct the earlier model):
  std's function-type sites are not sites at all (throws/fn_types.rs `site_module`: std only
  for PROXY_MODULES' traps). In LAZY, for sort_comparator_any, every std/prelude/sort
  `*.sort`/`*.toSorted` has throws None with 0-2 srcs — so lazy does NOT carry the comparator's
  errors through sort's error type; another mechanism does (look at how lowering/sema handle a
  throwing comparator of an any[] sort: proxy any_op throws in the app closure, then `sort`'s
  call…). Under units the same fns have throws None but 2 srcs, and lowering ICEs at
  toSorted's `out.sort(cmp)` (std/prelude/sort.velt:208). Next: VIR/HIR of that call in lazy vs
  units (is the call marked throwing? by what?), before designing anything.
  FOUND: lazy calls `T[].sortDynamic<any, Error>` / `toSortedDynamic` (std/prelude/sort.velt
  :232, generic error param E), picked by `FnCx::dynamic_sort_name` when the comparator
  `may_throw`, which depends on `uses_proxy()` = prelude_adt("Proxy").is_some().
- D2 `uses_proxy` / "Proxy loaded": under units ALWAYS true (std's unit has std/lang/proxy).
  Tried 067a77dd9 (units_proxy = program's code names Proxy/Reflect/ProxyHandler, used by
  uses_proxy, any_op, locked_props, model may_be_proxy, array_methods_open): fixed 4
  (generic_fn_*, any_function_properties_*) but broke 11 nothing_slot_* — REVERTED. Lazy loads
  std/lang/proxy also when a construct demands a hook of std/lang/hooks/proxy (arrayIndexKey,
  anyHasOwnProxy) — that module imports velt/lang/proxy — e.g. `null as any` into class slots.
  Right fix = lazy's "known too late" pattern: check with units_proxy = names; after the check,
  if the units live set (with module loading) loads std/lang/hooks/proxy or std/lang/proxy,
  check again with units_proxy = true (an `Assumed` flag across rounds, rounds.rs too_late).
- a149050f2 (mac-7de7, 2026-11-17): DONE. The real reason lazy had Proxy in nothing_slot_*: the
  LOADER loads std/prelude/global/proxy for a module with a nothing slot (`null!`, `null as
  any`; velt_loader globals::names_global), not a hook demand. Under units the loader gives all
  833 modules, so sema asks the loader's rule itself: names_proxy = names (Proxy, Reflect,
  ProxyHandler, PropertyDescriptor) || has_nothing_slot (moved to velt_sema, loader calls it).
  Plus the known-too-late round (Assumed::proxy, Ctx::units_live_proxy set by
  walk_units_program) when the live walk loads std/lang/proxy*/hooks/proxy; not seen to fire in
  the target files. val-14: 22 failing files (val-12 recounted: 26, with [--release] lines;
  the "35" earlier was a different count), no new; fixed generic_fn_rest_null,
  generic_fn_values_forms, any_function_properties_life/_mutex. val-14 has 89 entries vs 65:
  the extra 24 are [--release] runs of files already failing in debug (val-12 ran none).
  Clippy (rust 1.99) flags 3 pre-existing lints (generated/demand.rs:402/411, units/reach.rs:129).

## Throws seeding prototype (mac-7de7, 2026-11-17, uncommitted at writing; val-15 running)
- std's unit keeps the `throws` of fns that hold trap vars unsolved (FnSites::std_var_fns,
  throws/checks.rs check_all); the app stage solves them with its own. reseed clears `solved`
  and carries std's proxy_sites (else seeded app rounds run without trap vars). Each app round
  starts each trap var with its seeded set (Ctx::seed_trap_vars, called in units::
  check_definitions after app collect — NOT lib.rs check_definitions, which units skip);
  seeded traps are "fixed" (FnSites::fixed_traps): unify_parts binds nothing for them, and the
  assume-empty helpers assume the seeded set instead.
- Fixed under units: proxy_throws, _recheck, _typed, proxy_trap_throws_string,
  _through_helpers, sort_comparator_any, any_apply_list_throws. Rounds settle at 2 like lazy.
- Left: proxy_throws_catch (1 line: "rejected " vs "rejected set"), two_traps/settled/
  reason_* (VIR: generic instance named from the plain type, e.g. Array.map<{reason: never}>
  where lazy has E1|E2), db_rows_source_throws (ICE thrown outside throwing fn).
- DESIGN Q (for owner): lazy's seeded round has NO var at the trap (set written in the type).
  Full emulation = rewrite fixed trap vars to their seeded sets across std's frozen types at
  app round start (like readonly::erase at program build), incl. fn_sites.sites' types.
- Debug: VELT_UNITS_GROW_DEBUG=1 prints throws round state, failing exact/assumed vars, seeds.

- COMMITTED bbff24ced; val-15: 13 failing files (val-14 22), no new.
- Cost at bbff24ced (tools/ab7.sh: warm, one-line edit, instructions, median of 3, scratch
  copy of examples + packages/http-server): check hello lazy 0.36 / units 0.70 G, check
  todo_api 3.72 / 1.06 G, build hello 0.45 / 1.82 G, build todo_api 9.59 / 7.63 G. Builds look
  heavier than the earlier ~7.4 / 1.4-1.6 G, but those predate 5ed9c07f8: A/B vs b98c5957b owed.

- A/B b98c5957b vs bbff24ced (tools/ab-base.sh): no measurable cost from a149050f2/bbff24ced.
- OWNER DECISION (2026-11-17): the overlay. std's unit is never modified; each app build keeps
  'trap var -> seeded set' and applies it when reading std's types and naming generic
  instances. (Prototype's FnSites::fixed_traps is that map; it must reach instance naming.)
- hello build cost (owner: small programs mustn't pay for the unit): units build 1.84 G vs
  lazy 0.42 G, units check 0.70 G. Build-only: program build 42 ms (units::reach 18,
  build_defs 6.5, readonly erase 6; check 3 ms), generated::phase 13 + generate 7 ms (lazy's
  hello build has no generate phase), lower 7 vs 1.5 ms. VELT_UNITS_WALK_STD=0: only -0.04 G.

- a561cfcf3 OVERLAY: vars::plain_type writes a fixed trap's seeded set (not never); a seeded
  round clears var_sets.plain cached by std's unit. Fixed the 8 proxy_throws_* left.
- COUNTING FIX: count failing files with tools/failfiles.sh (both report forms: `<file> [mode]:`
  and error tests' `<file>: stderr missing`). Correct: val-12 35, val-14 31, val-15 22,
  val-16 14; 0 new at each step. (The 26/22/13 posted earlier missed error tests.)
- Left at val-16 (14): db_rows_source_throws (RowSource method group: std's readRows' throws
  settled before the app's impls; overlaps #3111, groups.rs), union_view_assigned/_live,
  default_export_of_import, catch_panic, function_call_this, channel_send_copies_resource,
  7 errors/http_server/* (messages differ).

- 51aff8e10 (loader identity while recording, reach roots, methods map), 99390ef20 (walk reads,
  cheap violation counting): hello build units 1.83 -> 1.26 G, check 0.70 -> 0.58 G; todo_api
  build 7.58 -> 7.05, check 1.04 -> 0.91 (tools/ab7.sh). val-19: 14 files, none new.
- B: 6a85c914f (installed_std_root: no VELT_STD, <root>/toolchains/<v>/bin, its std, read-only;
  fallback test) + ba556028c (installer: read_only::set_tree, remove_tree on every removal).
  Gain small: hello check 0.58 -> 0.57 G (tools/b-cost.sh). Windows run by win-coordinator:
  read_only 3/3, std_root 9/9, install/remove/reinstall/prune clean, no leftovers.
  PR text owed: "a std edited by hand: writable again -> checked file by file; edited while
  read-only (forced): unsupported". Docs-sync issue with the PR.
- Loader path map (uncommitted at writing): resolve_imports' position() scan -> map. Profile
  samples in resolve_all 90 -> 18, but instructions unchanged (0.58 G): memory-bound, not
  instruction-heavy. val-20 running (lazy + units goldens).
- Left in hello's check (profile): decode of reached defs 27%, loader syscalls 19%, rounds 13%,
  copying std's context 12%.

## Speed ideas after M3 (discussed with the owner 2026-11-17, not started)
1. Load only the part of std's unit a program uses (hello under units 0.71 G vs lazy 0.36 G).
2. Skip parsing std's source when the unit comes from disk (hello parse 54 ms vs 27).
3. Run the expensive analyses on doubt only (walk_std; the throws rounds).
4. Library units (VELT_UNITS_LIB, D6) on: dependencies checked once.
5. Check independent units in parallel (sema's main check is single-threaded per unit).
6. Cache lowering/codegen per module (todo_api: post-sema cost ~= lazy's; std objects are
   already cached by veltc driver/std_cache).

## Merge of origin/main into units-passes (mac-7de7, 2026-11-17 night)

Why a merge, not a rebase: 13 of the branch's commits were squash-merged into main as separate
PRs, so a rebase replays 114 commits onto their own squashed copies and conflicts at every step.
`git merge --no-ff origin/main` instead: no history rewritten, a normal push. Backup ref
`backup/units-passes-pre-rebase-1` = ad44ef7de (the branch before the merge).

How the conflicts were resolved (tools/conflict-origins.sh names the main commit of each hunk;
tools/resolve.py <file> <o|t|b|r per hunk> applies the choice):
- Hunks where main only has the squashed copy of this branch's own work: the branch's version,
  after diffing the PR's branch head against main's squash to keep any review changes (build
  cache fixes + tests, a units test).
- Hunks from other PRs (glue bodies, prebuilt hook modules #3053, object_widen #3082, closure
  site defs #3054): both, hunk by hunk.
- named_outside_bodies: main added instantiation tracking; the branch keeps std's part in the
  unit. Merged: std's unit also keeps its share of the instantiations.
- Not flagged by git: main's `units.rs` next to the branch's `units/mod.rs` (main's was the
  branch's old version, unchanged in review): main's removed.

Fixes the merged code needed (the codec lists every field, so main's new fields failed to build;
each is a field main added that the unit must carry):
- units/codec/hir.rs: FieldDef `cell`; ExternFnDef/FnInfo (defs.rs) lost `system`; Intrinsic:
  PtrAtomic* (101-105) removed, 13 new numbered 189-201 (InspectNested, AddressAtomic*,
  RecordView*). NOTE: the codec's numbering changed meaning for 101-105 -> a unit on disk from
  before the merge must not be read: checked, the disk key hashes build_cache::compiler_identity().
- lazy_std::Lazy dyn_held, dyn_generic, cycle_closed, cycle_seen, cycles; ObjectImpls
  downcast_from, widen (+ codec for object_widen::{Widen, Ask}); Copies spread_after,
  type_spreads; Seen::Spread(f0); FnSites closure_defs, structural, var_values; FnTypeSeeds
  closure_targets; hook_stubs::Stubs declared; Ctx glue_bodies (save list).
- program.rs: main's `defs.into_vec()` on the already converted Vec -> `defs.into_iter()`.
- velt_loader globals/mod.rs: `Said` + `SPLITS` came in twice (main's copy of the moved code).
- Tools for the next merge: tools/ctx_fields.py (Ctx fields missing from the codec lists),
  tools/intrinsic_codec.py (Intrinsic variants missing, printed as numbered entries).

DHAT: prof/vm-p3 (todo_api units, before the merge): 6.84 M blocks, 1.10 GB, peak 168 MB; its
frames carry only base names (rerun with --fullpath-after=, tools/vm-dhat3.sh). prof/vm-p4 is
INVALID (old VM binary against the merged std); redo after the merge with a rebuilt VM velt.

Goldens after the merge (val-30): lazy all pass (6 suites ok, 1399 s). Units: 8275 failures over
4380 files, nearly all one ICE: "`std/process/child/exit_win32::exited` built by lowering but not
live". Cause: #3071 added std's first C export outside the scheduler (`@cExport("std_child_exited")`
in the Windows-only std/process/child/exit_win32.velt). Lazy std never loads that module on macOS,
so the export isn't in the program; the unit checks every std module, so Program::c_exports has
it, and lowering's seed_c_exports queued every non-scheduler export unconditionally: built (with
its HandOff code) in every program, and not in the reach set -> the live check's ICE.
Fix (velt_vir lower/c_export.rs): with a live set, seed only the exports in it (lazy std's live set
has every loaded module's export, lazy_std/mod.rs:403, so lazy builds are unchanged; a reached
`exportedAddress(exited)` still builds it on demand). Test: tests/live_superset.rs
an_export_is_built_only_when_live (fails with the same ICE when the filter is taken out).
Owed on Windows: a units build of a program spawning a child (child_win32 -> signaled ->
exportedAddress(exited)) must have `exited` in the reach set; not testable on this Mac.

val-31 (after the export fix): units 54 failures, 14 files: 6 new since val-29, all old tests
(any_in_builtins, class_objects, rwlock_read_caught_error, stream_pull_generator_held_closure,
stream_pull_generator_uncalled_generic, streams_any_reason). One cause, three checks: under units
they took facts of every std module, lazy std only of the modules it loads. Main's changes made
the difference show (#3000: the local-async graph keeps a closure's params of unknown origin
until every call that may reach it is wired; more methods = more calls that may reach it).
How found: tools/la-diff.sh (a temporary VELT_TMP_LA_DUMP in local_async printed the std
functions the graph takes; removed again). Pitfalls met: the build cache is keyed by content and
skips sema for a program built before, VELT_BUILD_CACHE=off or not (la-diff builds a fresh copy);
the first units build of a new compiler builds std's unit, whose stage takes all of std.
Fixes:
1. Live walk (body/lazy_std/live.rs): `Live::units_loaded` (the modules lazy std would have
   loaded so far) + `loaded_for(cx, m, owner)`: a live class's methods and an implementation's
   from a module not loaded aren't marked (a class's own methods always: its module is there
   with it); `units_loaded_more` marks them when the walk loads more. Culprit:
   std/test/suite's `extend<T> T implements Settles` ([SETTLE] on every type) and its Promise
   extension. Also C exports (lazy_std/mod.rs): only of loaded modules.
   The graph for stream_pull_generator_held_closure: lazy 1555 std fns, units 1783 -> 1336.
   Units still lacks ~230 lazy has (prelude date, any hooks, padStart...): an old emulation gap,
   unmasked, not a regression; harmless for these checks; watch for live-check ICEs.
2. weak_threads.rs: the app stage's std scans and facts filtered by units_live_set
   (`units_live_std`); was: std/task/signal.velt's Mutex<SignalObj> refused in unrelated programs.
3. ownership/locked/regions.rs subclasses(): under units, std subclasses outside
   units_live_set dropped (no instance in the program); was: rwlock_read_caught_error refused.
Pattern for the next one: an app-stage pass reading "all of std" from the unit is a lazy-std
mismatch; filter by units_live_set (or `loaded_for` inside the walk).

TODO (coordinator, 2026-11-18): measure codegen_cl cache-key cost on a float-printing hello, warm cache, main vs branch (b99fbf560 blake3 key). #3154 r2 measured ~11.7M of 16.8M codegen instrs in SHA-256 on main. If blake3 still shows, try a 128-bit non-crypto key of the canonical VIR.

## Allocations of a todo_api build from std's unit (DHAT, prof/vm-p5, at 6f1abfcc7)
VM velt-prof, system allocator (tools/vm-dhat-build.sh), tools/vm-dhat3.sh (--fullpath-after=).
7.28 M blocks, 1.19 GB, peak 180 MB. tools/dhat_top.py <json> N depth (depth 0 = by file);
prof/vm-p5/by-file.txt. By crate: velt_vir 61.7%, velt_sema 11.7%, velt_codegen_cl 10.2%,
velt_opt 9.4%, velt_syntax 4.2%, velt_loader 1.4%.
Top files: lower/bits.rs 13.2% (Bits, per block in the async spill analysis: overlap.rs
live_out/block_units/interference, flow.rs maybe_written), verify/init.rs 7.9% (Vec<Vec<..>> per
local and per block: uses_of, assigned_in, exposed, preds/succs), lower/cfg.rs 7.0% (push_stmt
grow_one, 366 MB moved: statement vectors growing), spill/flow.rs 6.8%, units/codec.rs 6.0%
(String/Box<Expr>/Vec<Expr> decodes), mangle.rs 4.0% (segment strings), numrep 2.7%,
syntax codec 2.7%, cleanup.rs 2.6%, vir.rs 2.5%, verify/dominators.rs 2.2%.
The verifier runs every build (after lowering; release also after the optimizer).
1. Bits inline up to 128 bits (SmallVec<[u64; 2]>): build todo_api lazy 9.80 -> 9.60 G (-2.0%),
   units 7.00 -> 6.88 (-1.7%), hello units 1.17 -> 1.16; checks unchanged (tools/ab7.sh, 2 runs
   each, interleaved). Note: the merge of main raised the baseline (todo_api units ~6.5 -> 7.0).
Next: verify/init.rs flat or inline vectors; cfg.rs push_stmt capacity; mangle segments.

## codegen_cl cache keys + build-cache SHA (2026-11-18, prof/vm-p6, vm-p7)
#2943 codegen_cl cache keys, measured as asked (VM velt-prof, callgrind, debug velt at e4023275c, warm `velt build` of a float-printing hello after a comment edit, lazy std, every build to the same -o):
- Branch (blake3 key, b99fbf560): compile_cached inclusive 1.07 M instrs for the 8 functions, blake3 ~0.8 M of it. Main measured (#3154 r2): 11.7 M of SHA-256 for the keys. So the branch's key is ~10x cheaper; a non-crypto 128-bit key would save <1 M more here: not worth it now.
- Total build 183.6 M instrs.
- Trap found while measuring: building to a NEW -o path hashes every file read again (vpm probe: the stamp beside the output carries the known hashes): +20.5 M (204.1 vs 183.6 M, SHA-256 over ~166 std files). Real edit-build loops reuse the stamp; benchmarks that use a fresh output per run pay it.
- Still SHA-256 in this warm build: ~12 M (6.5%), spread over build-cache chores (stamp hex keys ~2.0 M, collect_garbage_if_due ~2.2 M, probe name hashes ~2.2 M, probe::stat ~1.9 M inclusive). Lead for a follow-up: these could move to blake3 or skip on a warm hit. Profiles: branches/handovers/mac-e688-2943/prof/vm-p6 (fresh -o) and vm-p7 (same -o).
Also tonight: 3fad11a5a (Bits inline) + e4023275c (verifier lists inline): todo_api build from the unit 7.00 -> 6.89 G, lazy 9.80 -> ~9.65 G.

## 2026-11-18 morning commits
- 3fad11a5a Bits inline; e4023275c verifier lists inline; 5e6089ee7 + 1311407f5 vpm probe content_hash blake3 (+ docs/internals/README.md "Measuring compile time").
- Now (Mac ab7): hello build lazy ~0.46 G, units ~1.165 G; todo_api build lazy ~9.65, units ~6.89.
- Biggest gap left: hello from the unit is 2.5x lazy (1.17 vs 0.46 G): small programs still pay for the unit (owner rule). Next: profile hello units vs lazy (callgrind in the VM, same -o) and attribute the 0.7 G difference; earlier split: decode of reached defs 27%, loader syscalls 19%, rounds 13%, copying std context 12%.
- Allocation leads left (prof/vm-p5): lower/cfg.rs push_stmt 7% (statement arena), mangle.rs 4%, units codec String/Box<Expr> decodes 6%.

## Why hello from the unit is 3.8x lazy (VM callgrind, prof/vm-p8, at 1311407f5)
Warm `velt build` of hello after a comment edit, each mode to its own reused -o: lazy 174.4 M,
units 664.7 M instructions (gap 490 M). tools/vm-hello.sh, tools/cg_groups.py (self costs by
group; callgrind's INCLUSIVE costs double-count recursive functions, numbers above the total:
never sum those), tools/cg_diff.py (per function).
Gap by group (self): other 136 M (std::path Components parse + PathBuf eq ~17 M, generic
alloc/String clone), malloc/free 108 M, sema units codec decode 107 M, hash maps 47 M, syntax
codec decode 24 M, memcpy 20 M, readonly::erase 8.7 M, module_state 7.8 M, live walk 4.5 M,
generated demand 4.1 M.
Inclusive (non-recursive, usable): units::codec::unit::decode_unit 78.8 M; of it
module_state::facts::Facts dec 45.5 M (HashMap<DefId, Facts> insert 45.5 M), promise_copies
Facts dec 22.9 M. Span::dec 15.1 M self, Reader::uint_long 12.2 M self.
=> Decoding std's unit summaries wholesale (facts per std def, for all of std) is the largest
single item; a hello reaches a sliver of std. Next: decode those fact maps per def on demand
(keep the bytes and an offset index, like DefTable), starting with module_state facts (~7% of
the hello build), then promise_copies facts; then find the PathBuf comparisons (loader?).

### Tried and reverted: decoding module_state facts per def on demand (2026-11-18)
A LazyMap for StateFacts.facts (decode only the app's live std fns): hello CHECK from the unit
0.55 -> 0.61 G (+12%), builds unchanged. Two reasons: the scan now called units_live_set (an
extra live walk in a check), and the premise was wrong: the 45 M "inclusive" of Facts::dec in
callgrind was cycle-inflated (HashMap insert and the decode both 45.5 M). Never take a target
from callgrind inclusive numbers on this code; use self costs grouped (tools/cg_groups.py) or
the encoding's sizes.
Sizes of std's unit (VELT_UNITS_ROUNDTRIP=sizes on a check): 10.99 MB; defs 3.70 MB and info
2.83 MB (both DefTable, decoded per entry on first use), AST fragments 1.98 MB (decoded per
fragment), scopes 0.55 MB, fn_sites 0.28 MB, all unit_sums 1.20 MB (state_facts 0.19, value_scan
0.22, calls 0.19, groups 0.17, weak_scans 0.15). Eager decode of the unit: 12.9 ms.
=> The decode cost in a hello build is the std definitions (and their AST) the program REACHES
and decodes, not the summaries. The lever is reaching fewer: the program set (coordinator's
item C), e.g. why a hello reaches ~2.7-3 k defs (`VELT_UNITS_REACH_STATS=1`: "live walk: N defs
of M") when lazy std checks ~1.5 k functions; and the per-reach decode cost (Span::dec 15 M and
Reader::uint_long 12 M self in vm-p8: spans as varints).

### Tried and reverted: re-seeding generated::demand's calls from the unit (2026-11-18)
VELT_UNITS_TOUCH=1 on a hello `velt check` from the unit (it reports only on check, not build;
pipe its stderr to a file, not `head`: a closed pipe panics eprintln, exit 101): the app's check
read 4760 of 11935 std bodies and ALL 11935 infos; top site generated/demand.rs:428 (every
info) and :417 (2613 bodies), in record_calls. Cause: compute_with seeds `walked.calls` from
`unit_sums.calls` (every std body recorded, fields_from = the unit's end) only when
`walked.calls.done` is empty, but the app's stage starts from std's context, whose walked.calls
is std's own stage's (fields_from 0: every info's fields listed again; bodies std's stage never
recorded). Re-seeding when `walked.calls.fields_from < unit's` cut the reads to 2584 bodies and
4265 infos, but instructions did not move (ab7: hello check 0.55 -> 0.55 G, todo_api check
1.03 -> 1.06, builds within noise): the extra clone of the full Calls costs as much as the reads
saved. The touch report also said "0 entries decoded on demand" though decode_unit uses
Reader::over_unit (lazy tables): either the counter (thread-local decoded::N) is read on another
thread than the one decoding, or the entries are materialized another way. OPEN: settle which,
before any more "read less of the unit" work: if std's tables are in fact decoded whole, making
them lazy for real is the lever; if they are lazy, entries "read" are cheap and the decode cost
in vm-p8 (107 M self) is the reached defs' bodies plus the eager summaries.

### SETTLED: std's tables are decoded on demand (2026-11-18)
The "0 entries decoded on demand" above came from checks that BUILT std's unit in-process (no
"std's unit loaded from disk" line: plain tables, nothing to decode). With the unit really
loaded from disk (velt-units: "std's unit loaded from disk (10992945 bytes): read 2.0 ms,
decoded 12.4 ms"), a hello `velt check` decodes 2607 entries on demand in the app's rounds (331
bodies, 2276 infos). The report's later "23870 so far" (both tables whole) is the report's own
doing: touch_phase copies every definition to time it ("copying every definition (11936) takes
60 ms"), which decodes them. Rule: always confirm the "loaded from disk" line before reading a
touch report, and read only its first count.
=> Lazy decoding works. What a hello pays for the unit is (a) the eager part decoded at load
(12.4 ms: the summaries 1.2 MB, scopes 0.55 MB, fn_sites 0.28 MB, def_spans 0.10 MB, fn_shapes
0.07 MB, anon 0.04 MB, ...), (b) the 2.6 k entries it reaches, (c) the passes over them.
Next lever: make the largest EAGER parts lazy, per module or per def (scopes: per module, a
hello needs a few; fn_sites; value_scan; calls; state_facts), measuring each by instructions
(an earlier on-demand map for state_facts lost because its use added a live walk: decode lazily
but select by something already computed).

### Where a hello check from the unit spends its extra time (2026-11-18, tools/sema-passes.sh)
VELT_SEMA_TIMINGS, warm `velt check` of hello, median of 5, unit confirmed loaded from disk,
compiler 5e6089ee7 (Mac, busy machine: ms are indicative, compare within a run):
check 24.5 ms lazy vs 42.3 ms units (+17.8). The gap is spread over whole-program analyses that
lazy std makes near free (under 1 ms) but units runs over the live std code it walks:
ownership::infer_local_async 0 -> 4.3, ownership_passes 4.6 -> 8.3, module_state::
check_locked_access 0 -> 2.5, throws::infer_all 0 -> 1.8, units: app 1.5 + app collect 1.1,
instantiation_cycles::check 0 -> 1.4, program: readonly erase 0 -> 1.3, rewrite:
recursive_aliases 0 -> 1.1, program build 1.1 -> 3.0. Lazy's own collect (5.3) and
check_bodies (6.6) are gone from the units side (std's unit), as intended.
=> No single big item left in a hello check; each analysis that walks std's live code for the
app (walk_std, units_live_set) needs std's part from the unit's summary instead (as
VELT_UNITS_WALK_STD=0 does for local_async, which lost correctness: see "Live walk" sections),
or a cheaper walk. Candidates by size: local_async 4.3, ownership 3.7, locked access 2.5,
program build 1.9, throws 1.8. Each is its own small design; the build side (lowering,
codegen) is shared with lazy.

### hello build cost, instructions vs wall (2026-11-18)
- Instructions are the measure; all agree: tools/fresh-o.sh (Mac, VELT_THREADS=1, comment
  edit): lazy 0.437 G same -o / 0.440 fresh -o; units 0.839 / 0.839 (the fresh-output trap
  does not apply to units builds here). ab7.sh's units 1.17 G edits a new `const z = 1;` (a
  definition, not a comment) and writes a fresh -o: comparable between its own runs only.
- VM per thread (tools/vm-hello-threads.sh, prof/vm-p9): lazy thread 02 72 M + thread 03 102 M;
  units 02 194 M + 03 454 M (+ 5.6 M, 9.9 M): no hidden background thread, both main threads
  grow.
- Wall clock on the busy Mac is NOT usable for this (CLAUDE.md pitfall): the same units build
  measured 56-66 ms total single-threaded in one run, sema 83-135 ms in the next while another
  session's heavy job ran; tools/wall10.sh (10 interleaved rounds) gave lazy 135 / units 204 ms
  median. Re-measure wall only on a quiet machine, interleaved, best of N.
- A units build reports "writes to frozen definitions" at exit (finalize.rs:80 4413,
  finalize.rs:120 2433, :150 319): finalize writes into std's frozen entries for every program
  (copy-on-write of entries it should not need to touch?). Worth a look: each write copies an
  entry out of the frozen unit.

### Lead: finalize::build_defs decodes (and writes) every std definition of the program
The build gap (0.40 G) is larger than the check gap (0.13 G) for hello from the unit. A units
build reports "writes to frozen definitions": finalize.rs:80 4413, :120 2433, :150 319.
build_defs loops over program_defs (std's reached definitions + the app's): `cx.defs[i]`
(decodes the entry: a whole body) and `&mut cx.defs[i]` to set f.throws (+ a generator's ret),
then the ExternFn / other defs at :120, :150. The check itself decodes only ~330 std bodies.
Open questions before changing it:
- Does std's own stage finalize its defs' throws (so the app's write repeats the same value)?
  If yes: skip std's frozen entries (i < unit_base under Stage::App) here, and the overlay rule
  (std's unit never modified) holds again. If no: store the finalized throws in the unit (std's
  stage finalizes its own), then skip.
- How many of the ~4.4 k program defs are lowered (lowering decodes those anyway)? The saving is
  the rest plus the writes. Measure: count lowered std functions for hello (VIR funcs) vs 4413.
Check with goldens from the unit (any throws difference shows as a golden diff) and ab7/fresh-o.

- bf129fc1b (2026-11-18): std unit carries its functions final throws (finalize_unit_fns, summary fn_throws); program build_defs skips std fns whose throws come out the same. Frozen writes at finalize: ~4.4 k -> 0 for fns (2433 non-fn rebuilds at finalize.rs:114 remain: next for the same treatment). NOT a speedup (hello units 0.837 -> 0.837 G). val-33 = val-32 (8 files).

### Next lead: the units BUILD path pays ~0.3 G its check does not (2026-11-18)
Instructions (Mac, comment edit): hello check lazy 0.42 / units 0.54 G; build lazy 0.44 /
units 0.84 G. Lazy's build costs about its check; the units build costs 0.30 G more than the
units check. `velt build --timings`: parse is the largest phase in both modes (~30 ms of ~60),
"16354 std lines loaded" in both; in the VM (prof/vm-p8 self costs) the units build decodes 3x
the cached AST of lazy (syntax codec decode 36 M vs 12 M). Hypothesis: the check path from the
unit uses std's module stubs (units_stubbed, the .stubs file) in place of std's ASTs, the build
path (VELT_UNITS_BUILD: `commands::check::with_all_std` scope) does not, and loads + decodes
std's modules from the build cache (and walks them) as lazy std does, on top of the unit.
To verify: count std modules loaded / AST decoded in a units check vs a units build
(VELT_UNITS_REACH_STATS, loader stats), then give the build the stubs as the check has.
Note `velt check` has no --timings; use /usr/bin/time -l instructions or VELT_SEMA_TIMINGS.
CORRECTION (same day): builds DO use std's stubs: `commands::check::with_all_std` sets
`scope.units` from VELT_UNITS, and `units_disk::prepare` keys the stubs off `scope.units`. So the
~0.3 G a units build pays over its check is not a missing-stubs load. Better explanation: a
units `check` builds only the app's definitions (`finalize::program_defs`, the check-only
path), a units `build` builds and lowers the whole program set, std's reached definitions
included, and that set (units::reach, ~2.7 k defs for hello: "live walk: N defs of M" with
VELT_UNITS_REACH_STATS=1) is larger than what lazy std checks (~1.5 k fns). That is the
coordinator's item C (the program set): measure how many std functions a units build lowers vs a
lazy build (VIR functions), and why reach keeps the extra ones.

## An app with three external packages (http-server, postgres, redis) (2026-11-18)
scratchpad deps-app (package.velt path deps on packages/{http-server,postgres,redis}; main.velt
an HTTP handler reading Redis, falling back to a Postgres query). tools/deps-ab.sh, compiler
bf129fc1b (debug, Mac, VELT_THREADS=1, comment edit, same -o, median of 3):
  check: lazy 10.445 G | std unit 1.596 G | VELT_UNITS_LIB=1 1.592 G   (-85%)
  build: lazy 18.766 G | std unit 10.003 G | VELT_UNITS_LIB=1 9.997 G  (-47%)
Both units modes give one 12.36 MB unit (std alone 10.99 MB): the dependencies go into it in
both (units::libraries on), so LIB=1 changes nothing here.
`velt build --timings` (default threads, unit confirmed "loaded from disk"): lazy sema 1134 ms,
lower 442, codegen 96 (5003/5003 functions from the build cache), total 1866 ms; from the unit
sema 106 ms, lower 318, total 637 ms (about 3x). Busy machine: units builds 0.86-1.1 s vs a lazy
1.9-2.8 s in other runs; a lazy build in between does not invalidate the unit. An earlier
interleaved loop showed units at 2.1 s without checking the "loaded from disk" line: unexplained,
probably not reading the unit; always confirm that line.
=> With deps, units pay off hugely; sema is no longer the cost. LOWERING is now the largest
phase (~320-440 ms, the same in both modes: all 5003 functions lowered every build, while
codegen already comes from its cache). Next big lever: cache lowering (VIR) per function or per
module/unit, as codegen is (idea 6 of the speed list).

## #93 (PRs) decisions, 2026-11-18
- Plan approved: P1 #3196 (vir allocs), P2 #3200 (blake3), P3 read-only std; U1-U4 opt-in (VELT_UNITS), U5 units by default right after U4; units-mode goldens in CI from U2 (coordinator handles CI).
- http_handler_changes_through_call: option (a) in U4: the unit's mutation summary carries per-method "changes this via" evidence; diagnostics must be identical between modes before U5. Until U4: on the units-mode known-different list.
- #3111 not merged yet (db_rows_source_throws waits).
- P1 #3196 APPROVED + queued (builder: actors/todo_api debug build -3.7..-4%, hello -0.09%). Optional follow-ups: smallvec as a workspace dependency; a warm test that reorders defs then needs a prelude def (catches a missing prelude_defs reset on splice).
- function_call_this (2026-11-18, coordinator): general mechanism. Loader records the due conditional globals on Session/SemaOptions (contract change, state in the U3 PR); a units-mode prelude lookup of a conditional global not due answers None. Test: one program per conditional global in both modes, results compared. File an issue: prelude_adt(...).is_some() as a semantic switch (~10 sites) is fragile; make it an explicit loader fact.
- catch_panic fixed (live walk keeps CatchUnwind across units_live_set restore), uncommitted.
- CI change (#3232, owner, 2026-11-18): a push to a PR starts NO CI; the coordinator starts checks with the ci:approved label after review. So the local gate is the only check before review.
