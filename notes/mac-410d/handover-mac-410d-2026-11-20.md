# Handoff: mac-410d, crew task #128 (sigx #379: B2(b), B2(a), the generic spread #943)

Written 2026-11-20 at the Mac wind-down. Taken over from mac-9dce's #51 on 2026-11-19.
Read this, then the latest #128 notes (`crew show 128`), then each PR's last review comment.

## PRs and issues I own

| What | PR or branch | Remote head | State |
|---|---|---|---|
| B2(b): a rest typed by a type parameter is inferred as a tuple; closures take a tuple rest's elements | #3226, branch `rest-tuple-infer` | d3a4836d2 (r6) | r6 **pushed and verified**, awaiting re-review (reply 7039386516). `wip/mac-410d/3226-r6` holds this handoff under `notes/mac-410d/`. |
| B2(a): an overload's callback with a rest typed by a type parameter, and checked function results | #3295, branch `overload-result` | 3325715f1 (r3) | **APPROVED** (r3). It carries #3226 r5 as its first commit (71d7587a4). |
| #943: a value of a type parameter spread into a call's rest of unknown values | no PR yet; `wip/mac-410d/943-spread` | 9559f909c | Coded; the last run had two failures, both fixed since, but unverified |

- **Done earlier:** #3276 and #3255 (B4) merged in train 10.
- **Docs-sync:** website#278 covers #3295. #943 needs a new docs-sync issue when its PR opens.
- **Comments I posted:** on #1674 (7034187974) and on website#278 (7034188351). They cover the async-callback limit that remains after #3295.

## Order of what's left

1. **#3226 r6: done and pushed** (d3a4836d2, one commit on 3b08c525d). The async state owns the rest tuple: `declare_async_locals` lays it out Owned, and `ctor.rs` stores the built tuple by value with its elements shared. The golden reaches the constructor path through `asyncFirst`, `asyncLength` and `restOf`.
   - **Verified** after a clean build: the golden matches Node at 1 and 4 threads (debug; `--release` through the harness), `velt_vir` tests pass, and the rest_tuple/closure/async_fn harness passes.
   - **Break checks:** the r5 constructor gives a use-after-free (134), and the r4 one an ICE (101).
   - **Left:** the coordinator's re-review. Handle any new items the same way, with `notes/mac-410d/r6_job.sh` for the run.
2. **#3295:** approved. After #3226 merges, rebase `overload-result` onto main, dropping its #3226 cherry-pick. Run the overload goldens and push; the coordinator queues it.
3. **#943 spread**, on `wip/mac-410d/943-spread` (9559f909c; the last commit only adds `notes/mac-410d/linux-builder-setup.md`), stacked on #3295 r3 (3325715f1). **Its rerun is pending:** the two fixes below are untested.
   - **Last run, at 98d6a6a5f:** the kit went 136 → 135 (`server/src/index.ts:220` fixed), and `VELT_GOLDEN=rest_tuple` passed. Three things failed, fixed in 8773b04ee but unverified:
     - `spread_type_param_rest` had a VIR ICE ("invalid projection Field(1) on Ptr"): the array source must go through its box. Fixed with `self.content(&src, t)` in `lower/array/spread_param.rs`.
     - The lock error was reported twice. It is now deduped by displayed type and span (`json/mod.rs` `lock_sites`). A temporary `VELT_MUT=2` eprintln traces where the second TyId comes from; the job prints it. If the HIR holds the spread twice, fix that instead.
     - `overload_rest_result`'s `ping` call hit #3296 (a handler with fewer parameters than `handler(rq, input: S)`). The golden now writes `(rq: Ctx, input: void)`; Node is unchanged and tsc accepts it.
   - **New golden:** `lang/errors/spread_type_param_refused.velt` (no `.err` yet). It covers an array literal (tsc accepts it, Velt refuses), fixed parameters (TS2556) and an unbounded `T` (TS2488).
   - **Then:**
     - run `notes/mac-410d/spread_job.sh` (it does a `cargo clean -p` first);
     - record the two error goldens' `.err` files;
     - remove both `VELT_MUT` switches (the tuple one in `spread_param.rs` is the break check: the job runs it with `VELT_MUT=1` and the goldens must fail);
     - precheck, then squash with `notes/mac-410d/commit-spread.txt`;
     - delete `notes/` before the PR;
     - open the PR stacked on #3295 with `notes/mac-410d/body-spread.md` (fill in DOCS_ISSUE after filing it on velt-lang.github.io);
     - post the self-check and gate result as the first comment.
   - **Kit:** `:230` stays (throws inference: a `SigxError` thrown into a function type written without `throws`). That is outside the spread.
4. **After the spread:** #1674 item 1, calling an async function through `any`. sigx's `serverFn` handlers are async. #3295 refuses async callbacks at compile time until then (`overload_call.rs` `async_through_cast`). A generic forward of the callback still panics at run time; the repro is in my #1674 comment.

## Open review items and promises

- #3226 r5 review (the last comment on #3226): R2a and R2b as above. Its non-blocking follow-ups stand: the forward use of `g` in a rest-arrow group (the coordinator files it), and `#args` in the message (#3296).
- The coordinator asked that the "what the sigx kit still needs for async" note be kept as the next step after the spread: #1674 item 1.

## Local state on the Mac

- **Worktrees:** `~/dev/velt/branches/rest-tuple-infer` (at d3a4836d2), `overload-result` (at 3325715f1) and `spread-param` (at 9559f909c). Everything is pushed; these are only conveniences.
- **Build target `~/dev/velt/branches/mac9dce-target` (5.8 GB), kept.** It serves all three worktrees. **Trap:** cargo reuses the other worktree's crates, giving a stale sema and test binaries with the other worktree's paths (precheck's `hook_replaces` then fails falsely).
  - Before building another branch there, run `cargo clean -p velt_common -p velt_sema -p velt_vir -p veltc`, and `cargo clean -p velt_common` before a push.
  - Env: `CARGO_TARGET_DIR=…/mac9dce-target CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 VELT_STD=$PWD/std SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk VELT_BUILD_CACHE=off`.
- **sigx kit:** `python3 ~/dev/sigx/resume-kit/kit.py <velt> <sx dir> <out.json>`, run from inside the sx dir.
  - The sx copy is at `~/dev/velt/branches/handovers/mac-410d-128/sx`, with `kitdiff.py` and the reference results `kit-b2a.json` (136) and `kit-spread.json` (135).
  - Compare with `python3 -I kitdiff.py old.json new.json`.
- **Node and tsc:** `node file.ts` (Node 26 strips types); tsc is `~/dev/sigx/core/node_modules/.bin/tsc --strict --noEmit --target es2022`.

## What I'd tell myself

- Keep `crew watch` armed and re-arm it every time it ends. It lapsed for 9 hours once, and two reviews sat unseen.
- Start heavy jobs only on a go addressed to you by name. mac-9a21's own slot is lent only after mac-9a21 says it is free.
- Gate every claim on a clean-built binary of *that* worktree (the trap above). Twice a result came from the other worktree's build.
- The reviewer runs break checks itself. Make each new golden reach the new code path, and prove it fails with the change reverted before saying so.
