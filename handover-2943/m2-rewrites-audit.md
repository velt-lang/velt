# #2943 M2: running the source rewrites per unit (audit)

Tree: `/Users/andii/dev/velt/branches/d1-open-pr` @ 651e922bb (units-passes). Read-only audit; nothing built or run.
Paths below are relative to `crates/velt_sema/src/`.

## Precedent already in the tree

`std_load.rs::rewrite` already runs the `check_timed` pipeline over a **batch of std modules alone**
(call_sigs::check, namespaces::flatten, interface_merge, call_sigs::rewrite, generic_arrows,
overloads, recursive_aliases, nested_closures, ts_protocol::calls_throw) and merges the facts into
a live `Ctx` with `extend`/`|=` (`Ctx::add_batch`: span sets unioned, `ns_private` appended per
module, closure groups appended, `has_overloads` OR-ed, diags appended). It relies on the loader's
`std_index` rule ("what such a module adds stays local to it"). That is the shape the app's stage
needs: run the pipeline over the app's modules only, merge with std's saved part. It also confirms
that std needs no `asserted_targets`, `global_this`, `defined_accessors`, `defined_props`,
`module_chunks` (std has no top-level statements).

Facts of std in this tree (grep of `std/**/*.velt`, 903 files): no `namespace`, no `declare global`,
no `declare module`, no interface declared twice in one file, no call/construct signatures in
interfaces, no non-test nested function declarations. 5 function-type aliases
(`test/suite.velt Step`, `test/case.velt Body`, `lang/dom/doc.velt NodeFilter`,
`lang/event/event_target.velt EventListener`, `prelude/json/layout.velt JsonReplacer`). 147 interfaces with
`extends` lists (58 distinct base names). Whether std alone triggers `generic_arrows` / `overloads` /
`recursive_aliases` I could not tell by grep (std's lifted spans and method names in `std_key`
say; a timing run of hello with `VELT_TIMINGS` would too).

## Per-rewrite table (pipeline order)

Legend. *Scope*: what a module's output reads. *App→std*: can the app change a std module's output
(or what std's stage is checked with)? *Facts*: what reaches the context and how std+app parts
combine. *Cost*: copies when nothing changes / walks std's ASTs.

### 0. `script_locals::check` (wraps `check_timed`)
- Scope: root module only (`movable`, `with_moved`).
- App→std: no. Output is the root's items with refused script vars moved into `main`.
- Facts: none (the `Refused` names, root-only).
- Cost: when it moves anything, `with_moved` deep-clones **every** module's items (std included)
  and the **whole** `check_timed` runs again, all rewrites over all modules again; std's stage is
  rechecked unless `std_key` hits. Per unit: rewrite only the root, keep std's unit.

### 1. `module_chunks::split(modules, root)`
- Scope: per module (its own items, `scan_stmt` refs). Depends on `root` (the root is not split).
- App→std: no. std has no top-level statements (loader rule), so `split_module` returns `None` for
  every std module (shallow: finds no script `main` in std's items).
- Facts: `promoted` (span set of promoted consts; app spans only) + `diags`. Union / concat.
- Cost: if any app module splits, every module (std included) is rebuilt with
  `m.ast.items.clone()` (deep copy of all std ASTs). Walks std shallowly (top-level items).
- Per unit: easy. Run over app modules; `root` = app-relative index.

### 2. `asserted_targets::rewrite`
- Scope: per module; `has_asserted_target` returns false for `is_std` up front.
- App→std: no.
- Facts: `rmw` (span set, app only). Union.
- Cost: if any app module has `e! op= v`, **every** module's AST is cloned (`m.ast.clone()` in the
  par map, for unmarked ones too). Doesn't walk std.
- Per unit: easy.

### 3. `call_sigs::check`
- Scope: per module, filters `!m.is_std`. Diagnostics only.
- App→std: no. Facts: `merge_diags` (app). Cost: app only.
- Per unit: easy.

### 4. `module_destructure::rewrite`
- Scope: per module (top-level destructuring declarations).
- App→std: no (std's output depends only on std's items; std has none as far as grep shows).
- Facts: none.
- Cost: if any module destructures at module level, every module is cloned. Shallow scan of std.
- Per unit: easy.

### 5. `namespaces::global_namespaces`
- Scope: all modules: `declare global { namespace N }` → name → declaring modules.
- App→std: not today (std declares none). If std ever declared a global namespace, the map is still
  just a list; std's entries don't change with the app.
- Facts: `GlobalNamespaces` (name → module list). std part = empty. Combine: union of lists
  (`units::remap_module_facts` already remaps the indices).
- Cost: shallow over std.
- Per unit: easy; std table = std's part of the map (empty today).

### 6. `namespaces::flatten`
- Scope: per module, except `global_members(modules)`: the members of every `declare global`
  namespace in **all** modules, which each module's `declare global` namespace blocks see.
- App→std: only if a std module had a `declare global { namespace N }` block (then the app's members of
  `N` would be merged into std's flattened `N`). std has none, so no.
- Reads std while rewriting the app: std's global-namespace members (empty today).
- Facts: `ns_private` (per-module `Vec<Private>`; std entries empty) → concatenate per module;
  `diags` (app).
- Cost: if any module has a namespace, every module is cloned. Shallow over std.
- Per unit: easy; keep std's global members table (empty). Guard: a std module with a global
  namespace → fall back.

### 7. `interface_merge::rewrite` (+ `augment_scope::scope_into`) — HARD CASE
- Scope: `merge_repeated` is per module; `declare global { interface X }` merges into the
  module's **own** `X` (`augment_global`), never into std. But `declare module "spec" { interface X }`
  merges into the interface `X` of the target module, **following re-exports**
  (`find_interface`/`find_in`), and `scope_into` adds imports (and spec entries) **to the target
  module** and exports to the augmenting module.
- App→std: **yes**: an app module augmenting a std module (`declare module "velt/http" { interface
  Request { … } }`), or a module that re-exports a std interface
  (`declare module "./lib"` where `lib` has `export { Request } from "velt/http"`). The std
  module's interface gets the members, its items get synthetic imports of the app module, and
  its `imports` list grows: std's unit would then depend on an app module.
- How common: none in `tests/golden` or `examples` (all 7 `declare module` blocks target relative
  app modules: `lang/interface_merging`, `lang/module_augment_*`, `lang/_augment_scope/*`,
  `lang/_interface_merging_defaults/ssr`). std itself needs no merge.
- Reads std while rewriting the app: for an augmentation, the target module's interfaces and its
  re-export edges (`find_in`), and its exported names (`augment_scope::exports`).
- Facts: diags only.
- Cost: if any module needs it (an app interface declared twice is enough), **every** module is
  cloned. Shallow over std.
- **Today's cache is unsound here**: `Found::std_key` does not cover the rewritten std modules,
  so a std unit cached from another program is reused for one that augments std, and a unit
  checked over `std_unit::copies` of an augmented std is kept and reused for later programs that
  don't augment it.
- Recommended: detect before checking (an app `Augment` block whose target resolves, through
  re-exports, into a std module) and run that check as a whole check (`Stage::Both`, no saved unit,
  no `std_unit::keep`). Long term, TypeScript's semantics would want augmenting std interfaces
  as an app-stage extension of a frozen interface's layout ("one layout for the whole program"
  makes that a real design change); not needed for M2.

### 8. `call_sigs::rewrite` — HARD CASE (a name leak, really a bug)
- Scope: `function_names(modules)` collects, from **non-std modules only**, by **bare name**
  (no imports considered), the names of function-type aliases, single-call-signature interfaces
  and interfaces extending them. Then **every** module (std included) has each interface whose
  `extends` names one of them (`names_function`: one-segment path, type args ignored) rewritten
  into a type alias of the intersection.
- App→std: **yes**: an app `type Add = (…) => …` (or `interface Add { (…): … }`) turns std's
  `export interface Arithmetic<T> extends Add<T>, Sub<T>, …` (`std/ops/operators.velt:182`) into a
  type alias. **It happens in a golden**: `tests/golden/async_dispatch/fn_values.velt:19`
  (`type Add = (item: string) => Promise<i64>;`), whenever `ops/operators` is loaded (always with
  units, which loads all of std). I checked all 58 base names std interfaces extend against
  `tests/` and `examples/`: `Add` is the only hit.
- Since std's own names never enter `function_names` and std has no call signatures, std's
  output without the app is "unchanged".
- Reads std while rewriting the app: nothing (std's function aliases are deliberately not
  collected, so an app `interface I extends EventListener` is NOT made callable: a separate gap).
- Facts: diags (app).
- Cost: if any module (app) has a callable interface, every module is cloned. Shallow over std.
- Also unsound for today's std cache (key doesn't cover it).
- Recommended: fix it rather than key it: don't apply the app's names to std modules (`is_std`
  modules use an empty `fn_bases`; nothing else changes for std), or properly scope
  `function_names` by each module's own declarations and imports. Then std is app-independent.
  If the app should see std's function aliases too, the std table is "std's exported
  function-type aliases / single-call-sig interfaces, by (module, name)".

### 9. `generic_arrows::lift` (`typed::GenericFnTypes`, `local`, `recursive`)
- Scope: per module, given `GenericFnTypes`, built over **all** modules when any module has an
  alias of a function type (std has 5, so always): per module its type aliases and its named
  imports (`imports`: local → (module index, exported name)), and a global `names` set (any
  module's aliases of generic-function/Named types and aliased imports).
- `names` is only a pre-filter (`may_declare`); the decision `typed_function` resolves through the
  module's own aliases and its imports (`alias()` follows import edges, `readable_in` checks names
  are imported). std can't import app modules, so **std's output doesn't depend on the app**.
- Reads std while rewriting an app module: the alias declarations (AST, substituted at the use)
  that an app's imports reach in std, following std's import / re-export edges (named re-exports
  only; `export *` isn't followed). Kept table: per std module path, its type aliases whose body
  is a (generic) function type or a one-segment `Named` (an alias chain), as AST, plus its named
  re-export edges. Small (5 function aliases today, plus chains).
- Facts: `local_fns`, `all_fns`, `unsafe_fns` (span sets). Union; `std_key` already filters
  to std's files.
- Cost: **walks every body of every module, std included, on every check** (`item_has_local` per
  item, the deepest pre-check walk besides nested_closures), and builds the table over all modules.
  If anything lifts anywhere, **every item of every module** is cloned (`lift_item` clones
  unchanged items).
- Per unit: needs the kept std alias table. Note the lookups index modules by path; with std's
  modules absent an app alias import silently resolves to nothing (no error), so the table must
  be complete or the result is silently different.

### 10. `overloads::rewrite` (+ `iface_overloads`)
- Scope: per module, plus two cross-module tables:
  - `exported_sets`: per module path, overloaded exported functions (name → signature count),
    following re-exports, so `import { f }` also brings `f#0..f#k`. Reads std when an app
    module imports an overloaded std function.
  - `class_overloads`: **per class name, over all modules** (`FxHashMap<String, …>`, later
    insert overwrites): overloaded method counts and base name, for `base_has_signature` (is
    `f#k` an override).
- App→std: only through `class_overloads`' name keying: an app class with the same name as a
  std base class could replace that class's entry and change `is_override` of an overloaded
  `override` method in a std subclass. Requires std to have an overloaded override; rare,
  not seen. Fix: std modules look up std classes only (resolve by module/import, or two maps).
- Also enters std's stage: `install` gives std's stage the **app's** `methods` and
  `has_overloads`, and `std_key` hashes **all** method names (not filtered to std), so std's unit
  key changes when the app adds an overloaded method. These are pre-filters for overload resolution
  (`body/expr/overload_call.rs`, `call.rs`, `method_call.rs`, `static_call.rs`, `names.rs`);
  std's bodies only see std types, so std's own part should suffice (to verify: compare a unit
  checked with and without an app's overloads).
- Kept std tables: per std module path, exported overload sets (name → count) after re-exports;
  per std class name, (overloaded method counts, base name).
- Facts: `methods` (set, union), `has_overloads` (OR), `sig_spans` (span map, union; used by
  `report_misfits` after each round), `adapted` (set, union; `check_identity` after each round).
  std's part of `sig_spans`/`adapted` either saved with the unit or skipped in the app stage (its
  misfits were reported in std's stage).
- Cost: if any module has overloads, every item of every module is cloned. Shallow over std
  (items and methods).

### 11. `recursive_aliases::rewrite`
- Scope: strictly per module ("declared in the same module": `Expander::new(&m.ast.items)`,
  `cyclic_aliases(items)`).
- App→std: no.
- Facts: `alias_blocked` (span → reason map). Union; `std_key` already filters std spans.
- Cost: `cyclic_aliases` walks the type expressions of every type item of every module (medium,
  not bodies); if any alias is rewritten, every module is rebuilt (unchanged items cloned).
- Per unit: easy.

### 12. `global_this::rewrite`
- Scope: sites only in non-std modules (`names = !is_std && mentions`). Cross-module parts:
  `consts`: `keys::string_consts` of **every** module (top-level `const K = "k"`), read by
  `visible_consts` for an app module's imports (by name; export not checked); `globals`: the
  loader's `globalThis.K` aliases (`declare global { var K: T }`) from any module.
- App→std: a slot item `let globalThis.K` is pushed into its home module: the first asserting
  site's module (always app) or, for an unasserted key, the module of the `declare global { var K }`.
  If **std** declared such a global, an app site would push the slot into a std module. std has no
  `declare global`, so no; guard for it (fall back if a std module has a `globalThis.` alias item).
  The `declared_type` items are removed from every module, std included (none in std).
- Reads std while rewriting the app: string constants of std modules the app imports. Kept
  table: per std module path, its top-level string constants (name → string).
- Facts: `GlobalProps` (sites, homes, defined, write_only: all app; `remap` already drops non-std
  in std's stage) → std part empty; app part remapped by `Order::in_app`.
- Cost: if any app module names `globalThis` (33 goldens), **every** module is cloned, and
  `string_consts` runs over every std module (top level). `typed(m)` checks std's items (shallow).
- Per unit: easy with the string-constant table.

### 13. `defined_accessors::rewrite`
- Scope: per module; `wanted` short-circuits on `is_std`.
- App→std: no. Facts: diags. Cost: if any app module defines accessors, every module is cloned;
  std not walked.
- Per unit: easy.

### 14. `defined_props::rewrite`
- Scope: scans non-std modules; adds brand fields **by type name** to types of non-std modules
  only (`out.iter_mut().filter(|m| !m.is_std)`), hoists keys in the site's module.
- App→std: no (a brand on a value of a std type is not added to the std type at all: a
  feature gap, not a units problem).
- Facts: `Brands` (sites, direct: app only; `remap` drops them in std's stage). std part empty.
- Cost: if anything is added, every module is cloned; std not walked.
- Per unit: easy (by-name across app modules only).

### 15. `nested_closures::rewrite`
- Scope: strictly per module (each body's locals).
- App→std: no.
- Facts: `diags` (concat), `kept` (span set, union), `groups` (`Vec<ClosureGroup>`, appended;
  `set_closure_groups`/`add_closure_groups` number groups in order, so std's first then app's
  works, as `std_load` already does).
- Cost: **walks every body of every module, std included, every check** (`Scan::item_bodies`
  until it finds one); std's non-test files have no nested function, so the full walk always
  runs over all of std and finds nothing there. If any app module has one, **every** module is
  rebuilt and the rewriter (`Rw::item`) walks every body of every std module again.
- Per unit: easy.

### 16. `source_scans::program` (`Found`: null_proto, ctor_reads, record_accessors, gen_throw) — HARD (keyed) CASE
- Scope: app modules only (`!is_std`).
- App→std: **yes, through the flags**: they are installed in std's stage and change how std's
  bodies are checked/lowered: `null_proto_records` (`structured_clone.rs`, `in_keys.rs`,
  `symbol_keys.rs`, `any_text.rs`, `object_builtins.rs`), `record_accessors` (`object_own.rs`,
  `symbol_keys.rs`, `in_keys.rs`, `record_call.rs`, `ownership/local_async/types.rs`),
  `ctor_reads` (`ctor_objects.rs`: the prelude's `anyConstructor` dispatch over the program's
  classes), `gen_throw` (`body/generators.rs`). Already in `std_key`.
- How common: `Object.create(null)` in 50 goldens/examples, `.constructor` in 18,
  `defineProperty` in 55, `.throw(` in 7 (of ~4,500 golden files): common enough that
  a single saved unit would miss often.
- Recommended: for M2 keep them in the key (at most 16 variants, in practice 2-3; save one
  file per variant or rebuild the variant). Better: record during std's stage whether any std
  body read each flag (and which definitions), and key the saved unit only by the flags read; long
  term, move the flag-dependent parts into the app stage (re-check the few std bodies that read a
  flag; `ctor_reads`' dispatch over the program's classes belongs to the app anyway).
  Note: `std_load` already ORs a loaded std batch's own `calls_throw` into `gen_throw`, while
  `check_timed` scans the app only: decide which std's unit uses.

### Other things before `new_cx` that touch every module
- `kept::Facts::of` (only when `kept::wants_facts`: a kept check / reload probe): hashes the
  `Debug` text of every module's AST, std included.
- `std_unit::copies(modules)`: deep-clones every std module's (rewritten) AST whenever the cache
  is held, **also on a cache hit**, where the copies are just dropped. For a saved unit, std's
  modules come from the unit (or are not needed at all).
- `Order::of` / `Found::std_key`: cheap (std_key filters span sets by std files).
- In `new_cx` for the app: `Order::add_app` recomputes `collect::exports::import_targets` over all
  modules' import lists (cheap); `found.install(cx, true)` + `units::remap_module_facts` install
  all facts (std and app).
- `par::module_weight`: span arithmetic only.

## What a per-unit pipeline needs

1. Run the pipeline over the app's modules only (root index app-relative), with std's kept tables
   standing in for std's modules where a rewrite reads across modules:
   - generic_arrows: std's function-type aliases (+ alias chains) by (path, name), with std's
     named re-export edges;
   - overloads: std's exported overload sets by path (after re-exports); std's class overload
     counts and bases by class name;
   - global_this: std's top-level string constants by path;
   - namespaces: std's global-namespace members (empty);
   - interface_merge: only needed to detect an augmentation targeting std (std's interfaces and
     re-export edges by path), which then falls back.
   All lookups in these rewrites key modules by path and silently skip what they don't find, so a
   missing table changes results without an error: add a test that runs the pipeline per unit
   and over the whole program and compares (outputs and facts) over the goldens.
2. Save std's facts with the unit (they're std-only already): `ns_private` std part, lifted
   spans, `methods`/`has_overloads`/`sig_spans`/`adapted`, `alias_blocked`, closure `kept`/
   `groups`, merge diags. Combine with the app's: span sets and maps union, per-module vectors
   appended in `Order` order, closure groups appended (std first), flags app-only.
3. Make std's output app-independent: fix call_sigs' name leak (8); resolve overload bases per unit
   (10); install std's own `methods`/`has_overloads` in std's stage and drop the app's from the key
   (10); guard (fall back to a whole check) on an app augmentation of a std interface (7) and on
   std ever having `declare global` vars or namespaces (6, 12); keep the 4 scan flags in the key
   (16).
4. Cost wins that come with it: no deep walk of std's bodies (generic_arrows scan, nested_closures
   scan) and no copies of std's ASTs (every rewrite clones all modules when it changes any; the
   std copies on a cache hit) on an app check.
