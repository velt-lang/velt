# HANDOFF mac-4c53 (2026-11-20, wind-down)

Crew task #127 (#2196 fs work) is done. Its line of PRs all landed: #3188, #3190, #3218, #3253, #3258, #3301, #3314.
What is left is one open PR, the allocator's step A.

## PR #3316: allocator step A (#2196)
- **Branch** `alloc-step-a`, head **c54a36fa0**, on main 9cb569834. Mergeable at the last check. No local-only commits.
- **What it does:** in a release build for a native target, an allocation of a run-time size and array growth go through two wrappers that velt_opt's `heap_calls` adds, `velt.heap.alloc` and `velt.heap.realloc` (`velt_vir::heap`). Their bodies are std's `heapAlloc` and `heapRealloc` (std/lang/hooks/heap.velt). velt_codegen_llvm (`runtime.rs` `Allocator::of`, `module.rs` `allocator_group`/`allocator_return_attrs`) gives them `velt_rt_alloc`'s and `velt_rt_realloc`'s allocator attributes and keeps them `noinline`. rt_abi.md "Memory" has the attributes table.
- **State:** round 1 of the review (the coordinator's, on crew #127) is pushed and answered on the PR: `arrayTooLong` stops like `outOfMemory` (flush, write, exit 101), with no panic, and the module doc nit is fixed. **It awaits the re-review.** Nothing else is promised.
- **Evidence** (in the PR description):
  - **arm64:** head vs base within +0.013% to -0.81% on every row.
  - **Break checks on arm64:** without the attributes, permutations +3.81%; with the wrappers inlinable, json/parse_value +4.20%.
  - **x86-64** (the coordinator's builder): flat, and the no-attributes build equals head there.
- **When it lands:** remove the worktree `~/dev/velt/branches/alloc-step-a` and its `target/` (~20 GB). Nothing in it is needed: the branch is on GitHub.

## Next on #2196's allocator plan (comments on #2196, 2026-11-20)
- **Step B (the debug allocator in std)** and **step C (the JIT's and WebAssembly's allocators)** wait until the runtime's own allocating Rust is gone. A `#[global_allocator]` calling std would be new runtime Rust, which #2196 rules out. The three `velt_rt_alloc`/`realloc`/`free` externs go after B and C.
- **Other #2196 leftovers from task #127**, both noted on #2196:
  - The Windows handle-relative recursive remove (`NtCreateFile` with a root directory). Its design is in my note on #2196 of 2026-11-19. Windows `remove` stays on `velt_rt_fs_remove_sync` until then.
  - #3260 item 4: the macOS x86-64 dirent offsets need an Intel Mac run.

## Rerunning the measurements (scripts in this branch's notes/mac-4c53/)
- **`alloc-ab.sh`:** the VM A/B, run on the Mac:
  `orb -m velt-prof bash -lc "VELT_ROOT=/Users/andii/dev/velt bash <path>/alloc-ab.sh"`.
  - It expects worktrees `branches/alloc-base` (detached at base), `branches/alloc-step-a` (head), `branches/alloc-noattr` (head with the two wrapper symbols removed from `Allocator::of`) and `branches/alloc-inline` (head with `f.inline = Inline::Default` in `heap_calls::wrapper_of`).
  - It builds velt and **velt_rt separately** (`--bin velt` alone skips the runtime library), then runs cachegrind once per row (twice for hashmap and closures).
  - Afterwards run `orb -m velt-prof -u root fstrim -av`.
  - ~25 min of builds + ~35 min of rows. It's a shared-slot VM turn: ask the coordinator and wait for a go by name.
- **`stop-alloc.sh`:** stops only that job in the VM. It selects by `/proc/<pid>/environ` too, because `pgrep -f` misses a cargo whose target dir is only in its environment.
- **`alloc-table.py <log>`:** makes the markdown table from the log. `vm-alloc.log` and `alloc-table.md` are the last run's.
- **`check-alloc.sh` and `check-3316r1.sh`:** the Mac checks (build, units, heap_hooks, all goldens, clippy, fmt), and round 1's heap_hooks run plus the panic mutant.
  - `probe_std` in heap_hooks.rs copies `$VELT_STD` when set, so a mutated std copy can run against the probe tests.
  - `caught-mutant.sh` runs the caught-limit program alone against a mutated std copy.
- **`mutants-alloc.sh` and `replace_once.py`:** mutants of heap.velt, each in a fresh copy of std (never the worktree). Output goes to a log; never pipe it into head.
- **twins/:** the Node twins of the new goldens (`array_runtime_sizes`, `array_alloc_runtime_past_memory`) and of #3314's `fs_win_write_trailing`.

## Local state
- **Mac:** worktree `~/dev/velt/branches/alloc-step-a` (target kept until #3316 lands, so a re-review fix builds incrementally). Nothing else of mine. The other worktrees under `branches/` are other sessions'.
- **VM (velt-prof):** nothing of mine (`tgt-alloc-*` and `alloc-out` removed, fstrim done).

## What I'd tell myself
- Start a heavy turn only on a go that names you. I once started on a "then mac-4c53" and had to stop it, which cost mac-60d0 time.
- The pre-push hook builds velt in the pushing worktree, cold if it has no target. Run fmt, std-lint and clippy by hand and push with `--no-verify` only when you have.
- User code can't import `velt/task/unwind`. To test `catchVeltPanic`, export a wrapper from a std probe module.
- When a std class overrides a std method, that method gets a slot in std. veltc's tests/units.rs relies on `URLSearchParams.getAll` staying slot-less: there is a comment in std at it.
