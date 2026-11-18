# VELT_UNITS=2 `velt check`: the parse cost and the second sema with a package

Worktree /Users/andii/dev/velt/branches/d1-open-pr, branch `units-passes` (HEAD 81af0f800), dev
profile (optimized + debuginfo), Mac, warm caches, each figure stable across 3+ runs. Absolute
times are about 0.5x the earlier figures (2.5–3 s parse). The machine was probably less busy; the
ratios hold.

Note: the worktree already had uncommitted diagnostic code in `crates/velt_loader/src/lib.rs`
(a `DIAG_CG` counter around `load_conditional_globals`). I left it as it was, byte for byte.
`target/debug/velt` was last built with my experiment patch, so rebuild before using it for anything
else. That patch is in the scratchpad: `my-experiment.patch` (it includes the pre-existing diff).

## Baseline (todo_api, VELT_UNITS=2)

    diag-loader: conditional globals 827 calls, 1431 ms, 43831 Words::of; 885 modules (53 non-std), 823 extra roots
    velt: parse     1555.9 ms
    velt: sema       736.9 ms
    velt: sema       734.7 ms
    velt: generate     4.4 ms
    velt: total     3031.9 ms
    velt: 131239 std lines loaded

wordfreq, VELT_UNITS=2: 825 calls, 7 ms, 825 Words::of; 833 modules (1 non-std); parse 113 ms, one
sema 689 ms, total 802 ms. Lazy todo_api: parse 77 ms, sema 205 ms, total 282 ms.

## 1. Parse: about 1.43 s of 1.55 s is the loader's conditional-globals scan, repeated once per extra root

Hypotheses ruled out:
- std does not bypass the std AST cache.
- std is not loaded twice.

Evidence: both programs load the same 832 std modules (885 − 53 and 833 − 1) and the same 131239
std lines. Without the conditional-globals scan, parse is about 125 ms for todo_api and about 113 ms
for wordfreq.

The code path:
- `with_all_std` (crates/veltc/src/commands/check.rs:356) adds the 823 std files as `extra_roots`.
- `load_program` (crates/velt_loader/src/lib.rs:223–225 at HEAD) does
  `for file in opts.extra_roots { add_extra_root(..); resolve_all(..) }`, so `resolve_all` runs 823
  extra times.
- `resolve_all` (lib.rs:425) ends every call with `load_conditional_globals` (lib.rs:439).
- `load_conditional_globals` creates its `own` OnceCell anew on each call (lib.rs:445). Any global
  that is not yet due is still in `self.globals`, so each call runs `globals::Words::of(src)`
  (splits the whole source into a HashSet) and `mentions_word` (an AST walk for `.prop` words) for
  every non-std module.
- Cost = calls × non-std modules: 827 × 53 = 43831 `Words::of` over about 10k lines of
  http-server and app code, about 1.43 s. wordfreq has one small module: 825 × 1 = 7 ms. That
  explains why only package programs pay.

**Fix (verified):** compute each non-std module's mention of a conditional global once. The
loader's set of modules only grows, so it can keep two fields:
- `cg_scanned: usize`: the modules already scanned.
- `cg_mentioned: HashSet<PathBuf>`: the global files that some non-std module mentions.

Each call then scans only `modules[cg_scanned..]` for the globals not yet mentioned. A global is due
when it is mentioned and its `when` module is loaded. This gives exactly the same result and module
order as now: 885 modules either way, and `cargo test -p velt_loader` passes 137 of 137.

- Measured: 827 calls, 3 ms, 53 `Words::of`. todo_api parse 1556 → 125 ms; wordfreq 113 → 104 ms.
- About 25 lines in `crates/velt_loader/src/lib.rs`, plus two field initialisers in `more.rs`
  (`Loader::new`).
- Alternative: in the extra-roots loop, drain the queue with `resolve_imports` only, then call
  `resolve_all` once after the loop. That is a smaller diff but changes the order of module indices.

## 2. The second sema: an offered generated-code check that fails, because the offer entry is write-once and shared with lazy mode

The two `velt: sema` lines are:
1. `check_offered` (crates/veltc/src/generate/mod.rs:144, recorded at :197): the program checked
   with the generated modules cached from an earlier build.
2. `rounds` round 1 (recorded as "sema" at about mod.rs:294), after the offer was dropped
   (mod.rs:64–75).

So these are two full `check_on_current_thread` calls, which is why VELT_SEMA_TIMINGS prints two
blocks. Ruled out: script_locals, Checked::Again, a second root, the package's roots, and the
generator host. The host check is a separate session; its `stage=Both found=0` check shows up only
when the generator actually runs.

What I instrumented (`diag-offer` in check_offered, `diag-gen` in generated::phase::run):
- **Units mode demands more instances.** The offered check returns
  `Demand(stale=false, 6 requests)`, all std/encoding/json/generated `writeJsonTop`/`parseJsonTop`.
  Units mode checks every std body, so its App stage demands 10 instances (keyed fingerprints).
  Lazy mode, which reaches only the app's code, demands 4. The offer holds lazy's 9 instances
  (4 with fingerprints plus 5 internal). 6 of units' 10 are missing, so the offer is rejected.
- **The rejected offer is never replaced.**
  - The offer key is `generate-offer-v3 + compiler_identity + absolute root path + require_main`
    (generate/cache.rs:113). Nothing about the mode or the content.
  - `store_offer` (cache.rs:161) calls `Cache::put_key`, which never overwrites a valid entry: for
    a pack, `pack.rs:119` (`if entries.contains_key(&key.0) { return true; }`); for a large entry,
    `files.rs:45` (`whole || write_atomically(..)`).
  - So whichever mode stores first after a compiler build owns the offer for good.
  - Shown: lazy stored 9 instances → every later units run did 2 semas, and units' store of 26
    was a no-op. After a rebuild where units ran first (26 stored), every later lazy run did 2
    semas instead (`sema 193.7 + sema 199.4`, `Demand(stale=true, 4 requests)`), and units ran
    with one sema.
- **The same flaw affects plain lazy builds.** This part is inferred from the code; I did not
  measure it. Once an edit changes which instances a program needs, or a generator file changes
  (`read_files` returns None), the offer is stale or missing. The new offer cannot be stored, so
  every later check pays the check_offered sema plus a full rounds sema, until the compiler changes
  or the cache is garbage-collected.

**Fix, two parts:**
- (a) Smallest, for this experiment (verified). Add the mode to the offer key:
  `cache::offer_key(&p, opts.require_main, opts.units || opts.eager_std)` (mod.rs:63), and in
  cache.rs:113 hash one more byte. Measured: units todo_api one sema, total 872 ms; lazy keeps its
  own offer (one sema, 278 ms); http_hello units 864 ms, one sema.
- (b) The real fix for the write-once flaw. Let `store_offer` replace the entry, for example with
  a `Cache::replace_key` that appends a new pack entry, updates the index even if the key exists,
  and rewrites a large file unconditionally. Or key the offer by content, which is harder because
  the demand is known only after the check. (a) alone still leaves edits paying 2 semas per check
  until the compiler is rebuilt.

## 3. End to end after both fixes (measured in the working tree, then reverted)

| todo_api `velt check -v` | parse | sema | total |
|---|---|---|---|
| lazy (unchanged) | 75 ms | 204 ms | 278 ms |
| VELT_UNITS=2 before | 1556 ms | 737 + 735 ms | 3032 ms |
| VELT_UNITS=2 both fixes, warm | 125 ms | 747 ms | **872 ms** |
| VELT_UNITS=2 first run after a rebuild (fills the AST cache and the offer) | 295 ms | 752 ms | 1179 ms |

Other programs with both fixes, VELT_UNITS=2: http_hello 864 ms, bank 808 ms, wordfreq 812 ms.

What remains of the cost is std's whole check in units mode: about 700 ms, about the same for every
program. The driver passes `std_key: None` (crates/veltc/src/driver.rs:404), so `std_unit::hold`
never keeps std's unit between checks. Keeping it (a std key from the std files and the compiler)
is the next step towards a units-mode total below lazy mode's.
