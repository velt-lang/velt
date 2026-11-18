# M2 inventory: what of std's frozen unit (`Ctx`) must be saved to disk

Branch audited: `/Users/andii/dev/velt/branches/d1-open-pr` at `1531a24b2` (WIP #2943 M1 step 6).
Read-only audit; no repository file was changed.

## How the app's check uses std's unit (facts the classification rests on)

- `lib.rs:497-518`: std's stage runs its own rounds (`rounds::run` with `Stage::Std`) over std's
  modules alone (`Ctx::over`, `std_unit::copies` when keyed), i.e. `collect_std` + decorators +
  fill bodies + `check_bodies` (`units.rs:32-41`) **and** `analyze_after_bodies` minus the
  generated phase (`lib.rs:664-671`): ownership, throws, record keys, json, struct_tuples, moves,
  locked, weak_threads, module state, ... Each of those passes writes its std summary into
  `unit_sums` when `units_stage == Std`. The context saved is the **last round's context after all
  of that**, then `freeze_defs()` and `unit_base = defs.len()` (`lib.rs:507-508`).
- `lib.rs:419-448`: every app round starts from `std_snap.clone()` and then only: resets
  `union_views.assumed`, `fn_sites.reseed(..)`, `held_borrows`, `objects.layouts.sealed = false`,
  `Order::add_app` (pushes app modules/scopes, `lazy.add_module`, recomputes `import_targets`, sets
  `root`), `generating::install` (sets `gen.input`/`gen.generate`), `Found::install(cx, true)`
  (`rewrite_facts.rs:35-70`: overwrites ns_private, global_namespaces, gen_throw, alias_blocked,
  generic_arrow_fns/all, unsafe_arrow_fns, has_overloads/overloaded_methods, objects.{null_proto_records,
  record_accessors, ctor_reads, brands}, global_props, recovered, promoted_consts, asserted_rmw,
  kept_nested, closure_groups/group_names; adds merge diags) and `units::remap_module_facts`.
  Note: `stack_base`, `loads` (None), `source_facts`, `body_records` are **not** reset there.
- `units.rs:50-66`: the app's collection then resets `groups`, `subclasses`, `primitive_statics`,
  `classes_below`, `marked_aliases`, `native_libraries` and sets `collected_defs`, `bodies_ok`,
  `shapes_done` (`collect/mod.rs:166-189`).
- The passes after the bodies in the app stage take their functions from `unit_base` on
  (`Ctx::unit_fns`, `ctx/mod.rs:33`; `moves/mod.rs:114`, `ownership/*`, `throws/infer.rs:23`, ...)
  and start from `unit_sums` instead of std's bodies. So std's HIR bodies are read by the app's
  stage mostly through: `program::build` (the final `hir::Program` contains every std def,
  `program.rs:208-275`), generated-code demand/binding over std's demands (`lib.rs:665`), dynamic
  tables (`any`, `dyn_members`, `upcast_tables`), `layout_merge`, `readonly::erase`,
  `module_state::build`, `lazy_std::live_defs`, and passes that still walk all `defs`
  (e.g. `ownership/finish.rs:27` over all `fn_values`, `object_copies::check`).

Legend: **P** persistent (save), **M** memo (drop, rebuild on demand), **C** per-check / transient /
empty after std's stage / re-installed by the app (don't save), **A** app-only (empty in std's
unit). Column "'m/Rc": `'m` = holds a borrow of std's AST/modules (transitively); `Rc`/`RC`/`OC`/`Arc`
= holds `Rc`, `RefCell`, `OnceCell`, `Arc`; `ast` = owns AST values (clones, not borrows).
"?" = uncertain.

## 1. Every field of `Ctx<'m>` (`crates/velt_sema/src/ctx/mod.rs:93-557`, 202 fields)

| # | field | type (short) | class | 'm/Rc | reason (where set / read) |
|---|---|---|---|---|---|
| 1 | modules | `Vec<&'m SourceModule>` | P (shape only) | 'm | std's module list (path, file, is_std, imports, ast). The app appends after it (`units.rs:139-151`). Everything below indexes modules by position. Needs re-linking (A) or a metadata-only replacement (B). |
| 2 | fn_shapes | `FxHashMap<Span, FnShape>` | P | – | set while checking std bodies (`body/expr/fn_shape.rs:42-230`); goes into `hir::Program::fn_shapes` (`program.rs:244`). Span-keyed. |
| 3 | root | `usize` | C | – | std's is 0; `Order::add_app` sets it (`units.rs:149`). |
| 4 | gen | `Gen<'m>` | C | 'm, Rc | std's stage takes no generator (`lib.rs:460`) and skips the generated phase; app's `generating::install` sets `input`/`generate` (`generating.rs:100-101`). `binding` is default after std. |
| 5 | ty | `Types` (TyTable + well-known ids + `inferred_fns`) | P | – | every std type; app interns after it. `TyTable.map` (intern map) is rebuildable from `kinds`. |
| 6 | defs | `DefTable<Option<hir::Def>>` | P | – | std's checked HIR (bodies set at `body/driver.rs:487`, getters, forwarders, hooks...). Read by app's passes and `program::build` (`program.rs:211`). The largest item. |
| 7 | info | `DefTable<DefInfo<'m>>` | P | 'm | per-def sema info (FnInfo/AdtInfo/EnumInfo/IfaceInfo/GlobalInfo). `'m` via `FnSource`, `extern_sig`, `*.decl`, `GlobalSrc.{ann,init,c_global}` (`defs.rs:127-532`). Read everywhere. |
| 8 | def_spans | `DefTable<Span>` | P | – | per-def span. |
| 9 | units | `bool` | C | – | set from opts each fresh context (`lib.rs:455`). |
| 10 | unit_sums | `units::Summaries` | P | – | std's pass summaries read by the app's passes (see section 1b). |
| 11 | units_stage | `Stage` | C | – | set per round (`lib.rs:433`). |
| 12 | unit_base | `u32` | C (derivable) | – | `= defs.len()` after std (`lib.rs:508`); recompute on load. |
| 13 | scopes | `Vec<ModuleScope>` | P | – | std modules' items/exports/namespaces/type_only...; std lookups by name (`Ctx::std_def`) and std bodies' scopes. |
| 14 | prelude | `FxHashMap<String, Item>` | P | – | prelude exports visible to the app (`type_meanings.rs:180`). |
| 15 | prelude_type_keys | `bool` | P | – | set in std's declare (`collect/declare.rs:423`), read for app names (`type_meanings.rs:177`). |
| 16 | aliases | `Vec<AliasInfo<'m>>` | P | 'm | std's aliases; `decl: &'m ast::TypeAlias` is expanded **at every use** (`alias_expand.rs:31-70`, `resolve.rs:457`). `used`/`expanding` are written during the app's check. |
| 17 | marked_aliases | `OnceCell<(Set,Set)>` | M | OC | reset by the app (`units.rs:61`); recomputed from every alias's AST (`record_keys.rs:130-143`). |
| 18 | bad_key_sites | `Vec<(Span,Span)>` | C | – | taken by `global_this::drop_site_errors` after each round (`global_this/mod.rs:361`, called `lib.rs:479`): empty after std. |
| 19 | extensions | `Vec<Extension>` | P | – | std's `extend` blocks (targets, methods, implements). |
| 20 | extension_names | `(usize, Map<String,Vec<usize>>)` | M | – | memo valid for the first `n` extensions (`body/expr/method.rs:256-263`). |
| 21 | impls | `Vec<hir::ImplDef>` | P | – | std's implementations; into `Program::impls`; indexes everywhere (`impl_index`, `ext_impls`). |
| 22 | iface_writes | `IfaceWrites` | P | – | written ifaces / setter sources by impl index (`iface_writes.rs:127-363`), read when the app writes through std interfaces. |
| 23 | part_ifaces | `PartIfaces` | P | – | interning of interfaces made for intersection parts (`intersection_members.rs:235-351`): the defs exist in std; dropping it would make duplicates. |
| 24 | synth_ifaces | `SynthIfaces` | P | – | interning by form + `upcasts` read by `upcast_tables` (program). |
| 25 | generic_fns | `GenericFns` | P | – | interning + `free_of_args`/`fresh`/`wrappers` go into the program (`program.rs:252-256`). |
| 26 | generic_called | `GenericCalls` | P | – | `Program::generic_dyn_methods` (`program.rs:251`); read by `any_iface_sites.rs:56,608`. |
| 27 | closure_constraints | `Option<Vec<..>>` | C | – | set/restored around one closure (`body/expr/generic_values.rs:173-179`). |
| 28 | adapter_uses | `Vec<(DefId,Span)>` | C | – | taken in `generic_override_sites.rs:298` (std's after-body passes): empty. |
| 29 | union_views | `UnionViews` | P | – | `by_def`/`keys`/`made` (`union_views.rs:78-180`); `assumed` re-set per round (`lib.rs:435`). |
| 30 | any | `Option<AnyDefs>` | P | – | the `any`/`unknown`/`object`/`function` defs and the any impl, made in `collect_std` (`collect/mod.rs:101`). |
| 31 | impl_index | `ImplIndex` | M | – | incremental index of `impls` (`infer.rs:526`, `indexed` count). |
| 32 | ext_impls | `Map<u32,usize>` | P | – | `collect/ext_impls.rs:53`; read by `find_impl` (`infer.rs:512-539`). |
| 33 | ext_by_iface | `Map<DefId,Vec<u32>>` | P | – | `collect/ext_impls.rs:71`; read `infer/ext_impls.rs:27`, `ext_exists.rs:159`. |
| 34 | ext_memo | `Map<(TyId,DefId),Option<FoundImpl>>` | M | – | cleared when `impls.len()` changes (`infer/ext_impls.rs:30-50`). |
| 35 | ext_exists_memo | `Map<ExistsKey,bool>` | M | – | same (`infer/ext_exists.rs:52-117`). |
| 36 | ext_memo_impls | `usize` | M | – | the memo's validity count. |
| 37 | in_blanket | `bool` | C | – | set/reset in one call (`infer/ext_impls.rs:325-412`). |
| 38 | impl_scope | `Vec<Vec<Bound>>` | C | – | swapped around one call (`infer.rs:672-676`). |
| 39 | anon | `Map<Vec<ShapeField>,DefId>` | P | – | interning of anonymous object defs by shape (`anon.rs:444-448`); app must reuse std's defs. |
| 40 | readonly_twins | `Map<DefId,(DefId,Option<Vec<TyId>>)>` | P | – | `anon.rs:458,494`; read by `layout_merge`, `readonly::erase` (program build). |
| 41 | any_unknown_views | `bool` | P | – | set by a body (`body/expr/coerce.rs:217`), read at erase (`readonly.rs:39,61`). |
| 42 | objects | `ObjectState` | P (mixed) | ast (in `brands.direct`) | `frozen`, `frozen_defs`, `hidden`, `layouts` (families/redirect/conflicts/parsed), `class_values`, `expansive_cut`, `record_fills` are std's (P); `null_proto_records`/`record_accessors`/`ctor_reads`/`brands` re-installed (`rewrite_facts.rs:50-53`); `layouts.sealed` reset (`lib.rs:442`). |
| 43 | brands | `Map<DefId,TyId>` | P | – | branded defs (`crate::brands`). |
| 44 | brand_keys | `Map<(TyId,TyId),DefId>` | P | – | interning (`brands.rs:171-198`). |
| 45 | generic_brands | `Set<DefId>` | P | – | `brands.rs:202`; read in `subst` (`anon.rs:88`). |
| 46 | unique_symbols | `Map<DefId,TyId>` | P | – | memo per const, but computed from the const's AST (`unique_symbols.rs:29`); `is_empty()` gates fast paths. Keep (cheap). |
| 47 | record_views | `Map<DefId,TyId>` | P | – | `index_signatures.rs:105-118`. |
| 48 | field_only | `Map<DefId,DefId>` | P | – | `collect::field_only`. |
| 49 | field_only_of | `Map<DefId,DefId>` | P | – | reverse of the above (15 files read it). |
| 50 | narrowed_fields | `Vec<NarrowedField>` | C | – | taken by `collect/iface_extends.rs:161`. |
| 51 | shapes_done | `bool` | C | – | app sets false/true (`collect/mod.rs:182,133`). |
| 52 | query_locals | `Vec<(String,TyId)>` | C | – | pushed/truncated around one annotation (`body/stmt.rs:729-736`). |
| 53 | bodies_ok | `bool` | C | – | app sets (`collect/mod.rs:178,185`). |
| 54 | sigq | `SigQueries<'m>` | mixed: `readonly_params` P, rest C | 'm (`deferred_sigs`) | `deferred_sigs` filled/emptied by `fill_deferred` (empty after std ?); flags transient; `readonly_params: Set<Span>` (std params declared `readonly T[]`) is read when the app calls them (`widen_fresh::readonly_array_arg`). |
| 55 | checking_unused_aliases | `bool` | C | – | around `check_unused_aliases` (`alias_expand.rs:153-184`). |
| 56 | primitive_guards | `bool` | P | – | set by a std body (`body/predicates.rs:424`), read by `throws/infer.rs:190`. |
| 57 | type_level | `type_level::State` | P (mixed) | – | `op_sites`/`op_modules`/`intersection_parts`/`converts`/`failed_sites` key std types (read by `type_level::lowered_ops` at program build, `anon.rs:85`); `key_kinds` memo (M); `alias_site`/`use_site`/`deferring`/`stuck`/`callable_params` transient (C). |
| 58 | writes_undefined | `bool` | P (cheap) ? | – | std modules don't set it except `resolve.rs:155`/`unions.rs:102` which set it unconditionally; read `undefined_args.rs:82`. |
| 59 | nothing_slots | `bool` | P | – | `body/expr/nothing_slot.rs:32`; `Program::nothing_slots`. |
| 60 | shaped | `Set<DefId>` | P (or derivable) | – | std types are all shaped; without it the app reshapes std types (`collect/shapes.rs:34,62`). Derivable as "every def < unit_base". |
| 61 | shaping | `Vec<DefId>` | C | – | stack (`collect/shapes.rs:46-53`). |
| 62 | laid_out | `Set<DefId>` | P (or derivable) | – | as `shaped` (`collect/shapes.rs:496-593`). |
| 63 | canon_memo | `Map<TyId,TyId>` | M | – | `anon.rs:142-213`. |
| 64 | canon_errors | `Map<TyId,Option<TyId>>` | M | – | `throws/sets.rs:51-57`. |
| 65 | counted_objects | `Vec<TyId>` | P | – | `Program::counted_objects`. |
| 66 | assigned_objects | `Set<DefId>` | C | – | recomputed whole in `finalize.rs:20` (program build). |
| 67 | changes_held | `Set<DefId>` | P | – | `ownership/local_async/record.rs:76,95` (std's fns in std's stage); `Program::changes_held`; read `ownership/many.rs:143`. |
| 68 | replaced_unions | `Set<DefId>` | C | – | set only in `readonly::erase` at program build (`readonly.rs:75`): empty after std. |
| 69 | module_slots | `Vec<DefId>` | C | – | recomputed each round `lib.rs:727`. |
| 70 | lock_free_callbacks | `Set<DefId>` | P ? | – | assigned in `module_state.rs:441` only `if props`: an app round without props keeps std's value into the program. |
| 71 | module_scripts | `Vec<DefId>` | P ? | – | `module_scripts::declare` (both stages, `module_scripts.rs:134`); read `module_scripts.rs:194`. |
| 72 | promoted_consts | `Set<Span>` | C | – | re-installed (`rewrite_facts.rs:60`), extended by app's `script_tdz` (`script_tdz.rs:37`). |
| 73 | asserted_rmw | `Set<Span>` | C | – | re-installed (`rewrite_facts.rs:61`). |
| 74 | state_refused | `Vec<DefId>` | P ? | – | `module_state.rs:481`, `body/driver.rs:357`; read only for the root module (`script_locals.rs:41`): std entries likely dead. |
| 75 | lazy | `lazy_std::Lazy` | P ? | – | per-module modes, subclasses, live set, hook flags. With `velt check`'s eager std (`lib.rs:452`, `commands/check.rs:275`) mostly flags; `add_module` per app module (`units.rs:146`). Under lazy std it is real state (live set read by `live_defs`, program build). |
| 76 | global_props | `GlobalProps` | C | – | re-installed + remapped (`rewrite_facts.rs:53`, `units.rs:204`). |
| 77 | global_cycles | `u32` | C | – | counter compared within one call (`body/driver.rs:301-318`). |
| 78 | fn_value_depth | `u32` | C | – | depth counter. |
| 79 | thread_local_uses | `Vec<(Option<DefId>,Span)>` | C | – | taken in `thread_locals.rs:13`. |
| 80 | typing_globals | `Vec<DefId>` | C | – | stack (`body/driver.rs:417-419`). |
| 81 | unions | `Map<Vec<TyId>,DefId>` | P | – | interning of union enums (`unions.rs:247-284`). |
| 82 | canon_keys | `Map<TyId,Rc<str>>` | M | Rc | `canon_key.rs:21-27`. |
| 83 | recursive_unions | `Map<u32,Option<TyId>>` | P | – | per alias; computing again needs the alias AST and makes types (`recursive_unions.rs:32-157`). |
| 84 | generic_recursive | `Map<u32,bool>` | P (memo) | – | `recursive_unions.rs:45-52`. |
| 85 | recursive_union_insts | `Map<(u32,Vec<TyId>),TyId>` | P | – | instances have defs (`recursive_unions.rs:160`); must not be made twice. |
| 86 | alias_names | `Map<TyId,String>` | P | – | display, `readonly::erase` (`readonly.rs:28`), generated spelling. |
| 87 | generic_overrides | `Vec<(DefId,String)>` | P | – | `collect/classes.rs:92`; read `any_iface_sites.rs:615`, `iface_call.rs:39,83`. |
| 88 | getter_only_setters | `Set<DefId>` | P | – | `collect/classes.rs:395`; read `body/expr/setters.rs:80`. |
| 89 | primitive_statics | `Option<Map<..>>` | M | – | reset by the app (`units.rs:59`). |
| 90 | abstract_methods | `Set<DefId>` | P | – | `collect/abstracts.rs:25`; read by `supers.rs:166`, `throws/conventions.rs:27`. |
| 91 | defined_setters | `Set<(DefId,String)>` | P | – | `body/expr/literal_iface.rs:290`. |
| 92 | display_params | `Vec<String>` | C | – | per definition being displayed. |
| 93 | defaults_checked | `Set<DefId>` | P | – | otherwise the app re-checks std defaults from AST (`body/defaults.rs:35`). |
| 94 | classes_below | `Map<(DefId,u32),Rc<[..]>>` | M | Rc | cleared by the app (`units.rs:60`). |
| 95 | fills | `Map<DefId,Fill>` | P | – | `collect/fill_defaults.rs:175`; read `collect/classes.rs:267` (app subclasses), `virtual_defaults.rs:156`. |
| 96 | default_throws | `Map<Span,Vec<ThrowSrc>>` | P | – | `body/defaults.rs:73`; read when the app omits a std param with a default (`body/expr/default_args.rs:28`). |
| 97 | object_impls | `ObjectImpls` | P | – | `made`/`sources`/`targets`/`assign_setters`/`kept_fns` (`object_ifaces.rs:124-218`); its `throw_checks`/`fresh_checks` are drained (C). |
| 98 | hook_adapters | `Map<DefId,DefId>` | P | – | `hooks.rs:120`; read `hooks.rs:52`. |
| 99 | field_defaults_checked | `Set<DefId>` | P | – | as `defaults_checked` (`body/driver.rs:85`). |
| 100 | closure_counts | `Map<String,u32>` | P | – | closure numbering by top-level name (`body/expr/closure.rs:471`); app's static copies of std methods continue it. |
| 101 | dropped_closures | `Map<String,DefId>` | C | – | within one rolled-back body check (`body/recheck.rs:325-344`). |
| 102 | fn_defs | `Vec<DefId>` | P (cheap) | – | every function def in order; `unit_fns` filters from `unit_base`, others walk all (`ownership/closure_names.rs:25`, `body/inline_markers.rs:20`). |
| 103 | fn_values | `Vec<(DefId,Vec<TyId>,Span)>` | P | – | read over all by `ownership/finish.rs:27`, `ownership/exclusive/mod.rs:370,623`. |
| 104 | frozen_elems | `Set<Span>` | C ? | – | std spans only read by passes over std bodies (`ownership/finish.rs:121-224` over unit fns). |
| 105 | loop_roots | `Map<Span,LocalId>` | C ? | – | as above (`ownership/derived.rs:248`). |
| 106 | frozen_reads | `bool` | P | – | gates the ownership pass's tracking (`ownership/mutation.rs:51-79`, `patch.rs:41`). |
| 107 | cpu_targets | `Map<DefId,CpuTarget>` | P | – | `body/gated.rs:112`; `Program::cpu_targets`. |
| 108 | groups | `Option<throws::Groups>` | M | – | reset by the app (`units.rs:57`). |
| 109 | literal_hosts | `LiteralHosts` | C | – | worklist drained (`body/literal_hosts.rs:62-81`). |
| 110 | provisional_slots | `Vec<(DefId,u32,Vec<ThrowSrc>)>` | P ? | – | not drained; `throws/mod.rs:195` iterates all of it in the app's throws pass. |
| 111 | literal_ifaces | `Set<DefId>` | P | – | read by display, infer, coercions (`display.rs:187`, `infer.rs:661`). |
| 112 | literal_structs | `Set<DefId>` | P | – | `any.rs:376,411`, `object_keys.rs:458`. |
| 113 | literal_display | `RefCell<Vec<DefId>>` | C | RC | display recursion stack. |
| 114 | iface_adapters | `Map<AdapterKey,Adapter>` | P | – | interning of made adapters (`iface_adapt.rs:494-529`). |
| 115 | dyn_iface_targets | `Vec<DynIfaceTarget>` | P | – | the app's `make_dynamic_iface_adapters` walks all from 0 (`iface_adapt_dynamic.rs:99-107`). |
| 116 | fn_views | `FnViews` | P | – | `made` interning (`dynamic_fn_views.rs:68-134`); `pending` drained; `Program::any_fn_views` (`program.rs:239`). |
| 117 | throw_checks | `Vec<ThrowCheck>` | C | – | taken in `throws/checks.rs:72`. |
| 118 | fn_sites | `throws::FnSites` | P (mixed) | – | `reseed` keeps "what std's check recorded (where types are written, std's sites and flows)" (`throws/fn_types.rs:201-216`); `solved_anons` read `anon.rs:640`. Span-keyed. |
| 119 | collected_defs | `usize` | C | – | app sets (`units.rs:54`). |
| 120 | iface_generators | `Vec<(DefId,String)>` | C | – | taken in `throws/checks.rs:77`. |
| 121 | promise_forwarders | `Vec<(DefId,TyId)>` | P | – | drained only in `finalize.rs:23` (program build), which std's stage never runs: std's entries reach the app's program build. |
| 122 | nested | `Vec<NestedItem>` | P | – | block-scoped items (module, lo, hi, name, Item) for name lookups in std bodies. |
| 123 | nested_locals | `Map<DefId,Vec<String>>` | P ? | – | `collect/nested.rs:225`; read when checking a nested def's body (`body/driver.rs:561`). |
| 124 | decorator_uses | `Map<DecoratorTarget,Vec<Decorator>>` | P | – | `decorators/mod.rs:511`; `Program::decorators`; read `decorators/known.rs:130`, `export.rs:195`. |
| 125 | c_exports | `Vec<hir::CExport>` | P | – | `Program::c_exports`; read `module_state_reach.rs:113`, `intrinsics.rs:113`. |
| 126 | enclosing_class | `Map<DefId,DefId>` | P | – | `collect/nested.rs:227`; access checks (`body/expr/access.rs:158`). |
| 127 | static_this | `Map<DefId,DefId>` | P | – | `collect/static_this.rs:44,57`; read on every static call (`access.rs:409`, `static_call.rs:41`). |
| 128 | static_copies | `Map<(DefId,DefId),DefId>` | P | – | `static_call.rs:272`. |
| 129 | static_copy_of | `Map<DefId,DefId>` | P | – | `body/driver.rs:45,456,592`. |
| 130 | copy_diags | `Vec<(DefId,Diagnostic)>` | C | – | taken at `body/driver.rs:55`. |
| 131 | copy_uses | `Vec<(Option<DefId>,DefId)>` | C ? | – | read with `copy_diags` (`body/driver.rs:43`). |
| 132 | unwraps | `Vec<Unwrap>` | C | – | taken `iface_from_object.rs:176`. |
| 133 | generic_arrow_fns | `Set<Span>` | C | – | re-installed (`rewrite_facts.rs:41`). |
| 134 | generic_arrow_all | `Set<Span>` | C | – | re-installed. |
| 135 | unsafe_arrow_fns | `Set<Span>` | C | – | re-installed. |
| 136 | alias_blocked | `Map<Span,(Span,String)>` | C | – | re-installed (`rewrite_facts.rs:39`). |
| 137 | kept_nested | `Set<Span>` | C | – | re-installed (`rewrite_facts.rs:66`). |
| 138 | late_consts | `Set<Span>` | C ? | – | read over unit defs only (`moves/mod.rs:198`). |
| 139 | generator_values | `Map<(DefId,Vec<TyId>),(DefId,TyId)>` | P | – | interning (`body/expr/gen_value.rs:51-88`). |
| 140 | closure_groups | `Vec<ClosureGroup>` | C | ast | re-installed via `set_closure_groups` (clears first). |
| 141 | group_names | `Map<Span,(usize,usize)>` | C | – | re-installed. |
| 142 | group_defs | `Set<DefId>` | C ? | – | read by `ownership/local_closures/mod.rs:114` over unit fns. |
| 143 | has_overloads | `bool` | C | – | re-installed (`rewrite_facts.rs:46`). |
| 144 | overload_sets | `bool` | P | – | `typeof_generic.rs:76`. |
| 145 | gen_throw | `bool` | C | – | re-installed. |
| 146 | overloaded_methods | `Set<String>` | C | – | re-installed. |
| 147 | jsx_providers | `Map<usize,Option<Rc<Provider>>>` | A ? | Rc | resolved per JSX module on first use (`jsx/provider.rs:90-106`); std has no JSX module. |
| 148 | jsx_refusals | `Map<usize,(String,Span)>` | A ? | – | as above. |
| 149 | callback_wrappers | `Map<DefId,TyId>` | P ? | – | `callback.rs:293`; read by `ownership/local_async/mod.rs:134,256,279`. |
| 150 | reported_captures | `Vec<(String,Span)>` | C | – | diagnostic de-duplication. |
| 151 | reported_mutations | `Vec<(String,Span)>` | C | – | same. |
| 152 | in_shared | `Vec<DefId>` | C | – | cleared `ownership/local_async/mod.rs:99`. |
| 153 | crossing_callbacks | `Vec<Crossing>` | C | – | cleared `ownership/local_async/mod.rs:98`. |
| 154 | ide | `Option<Box<Recorder>>` | C | – | `None` when compiling. |
| 155 | shared_memo | `Map<TyId,bool>` | M | – | `ctx/access.rs:121`. |
| 156 | kept_fns | `KeptFns` | P | – | `forced`/`parents`/`closure_free` of std fns read by the app's ownership (`ownership/kept_fns.rs:96-245`). |
| 157 | pure_fns | `Map<DefId,Option<Result<(),Impure>>>` | M | Arc (in `Impure`) | `body/pure_init.rs:397-411`. |
| 158 | kept_consts | `Vec<DefId>` | P | – | `kept_consts.rs:33`. |
| 159 | subclasses | `Option<Map<..>>` | M | – | reset by the app (`units.rs:58`). |
| 160 | symbol_keys | `symbols::KeyNames` | P | – | names given to symbol keys (`symbols.rs:203-405`): the app must name the same. |
| 161 | matching_context | `bool` | C | – | set/reset (`infer.rs:25-28`). |
| 162 | checking | `Vec<DefId>` | C | – | stack. |
| 163 | ret_checks | `Vec<RetCheck>` | C | – | taken `body/returns.rs:144`. |
| 164 | rec | `RecState` | C ? | – | pending/reported/completed of return inference; std's done. |
| 165 | overridden | `Set<DefId>` | C | – | written (`collect/ret_infer.rs:64`), never read anywhere. |
| 166 | stack_base | `usize` | C | – | an address on std's checking thread; **not reset** for the app copy (`lib.rs:466` only on fresh). Must be reset on load. |
| 167 | stack_budget | `usize` | C | – | per check. |
| 168 | held_borrows | `bool` | C | – | per round (`lib.rs:437`). |
| 169 | ns_private | `Vec<namespaces::Private>` | C | – | re-installed + remapped. |
| 170 | global_namespaces | `Map<String,Vec<usize>>` | C | – | re-installed + remapped. |
| 171 | held_borrows_used | `bool` | P ? | – | std's value carries into the app's `retry_sharing` (`lib.rs:624`). |
| 172 | moved_consts | `Map<(DefId,Span),usize>` | C | – | index into `diags`. |
| 173 | borrow_pass_errors | `bool` | C | – | recomputed `lib.rs:719`. |
| 174 | fresh_checks | `Vec<FreshCheck>` | C | – | taken `fresh_returns.rs:70`. |
| 175 | object_copies | `object_copies::Copies` | P | – | copies/writes/seen..., read by `object_copies::check`, `absent_fields`. |
| 176 | method_facts | `MethodFacts` | P | – | `Program::array_methods_open`/`any_call_names` (`program.rs:189`). |
| 177 | param_field_writes | `Vec<(DefId,TyId)>` | C | – | taken `struct_tuples/mod.rs:390` (summaries keep std's). |
| 178 | param_proxy_targets | `Vec<(DefId,TyId)>` | C | – | taken `struct_tuples/mod.rs:401`. |
| 179 | super_calls | `Set<Span>` | P | – | `Program::super_calls`. |
| 180 | type_defaults | `TypeDefaults` | P (memo, AST to rebuild) | ast (`resolving` idents, transient) | `resolved` defaults of std generics (`type_defaults.rs:121-168`); rebuilding reads `decl.generics` AST. |
| 181 | deferred_ts_returns | `Vec<TsReturnCheck>` | C | – | taken `ts_protocol.rs:196`. |
| 182 | diags | `Diagnostics` | P | – | "the app's copy has them" (`rewrite_facts.rs:33`); normally empty or warnings. |
| 183 | rechecks | `Vec<Frame>` | C | – | stack. |
| 184 | body_records | `Option<Map<..>>` | C | – | `None` unless a kept check; kept is off with units (`lib.rs:532`). |
| 185 | source_facts | `Option<Arc<Facts>>` | C | Arc | kept-check facts; irrelevant with units. |
| 186 | std_missed | `bool` | C | – | lazy loading. |
| 187 | kept_demand | `Option<Vec<..>>` | C | – | kept check only. |
| 188 | closure_defs | `Vec<DefId>` | C ? | – | rechecks' rollback bookkeeping (`body/recheck.rs`); std's entries only matter for rolling back std bodies. |
| 189 | widening_values | `Map<Span,u8>` | P | – | read for a std const's initializer span (`widening_consts.rs:207`). |
| 190 | js_api_modules | `OnceCell<Map<usize,..>>` | M | OC | computed from prelude modules' AST (`js_api.rs:48-60`); not reset by the app. |
| 191 | native_libraries | `OnceCell<NativeLibraries>` | M | OC | reset by the app (`units.rs:62`). |
| 192 | decorator_scopes | `RefCell<Map<usize,Scopes>>` | M | RC | per module from its AST (`decorators/scope.rs:34`). |
| 193 | literal_types | `Map<DefId,Map<Span,TyId>>` | C ? | – | decisions for re-checking a body (`body/driver.rs:583`); std bodies are not re-checked. |
| 194 | literal_found | `Map<DefId,LiteralLocals>` | C ? | – | same. |
| 195 | recovered | `Map<usize,Set<String>>` | C | – | re-installed + remapped. |
| 196 | poisoned | `Set<(FileId,String)>` | A | – | only failed imports / TDZ (`collect/imports.rs:100`, `script_tdz.rs:52`); std's stage runs no `script_tdz` and std's imports resolve. |
| 197 | export_memo | `RefCell<exports::Memo>` | M | RC, Rc | export tables per module, built lazily from module ASTs; cleared by `Memo::changed` (`exports.rs:69`, `default_snapshots.rs:36`). |
| 198 | import_targets | `Vec<Map<&'m str,usize>>` | C | 'm | recomputed by `add_app` from `modules[..].imports` (`units.rs:148`). |
| 199 | loads | `Option<Loads<'m>>` | C | 'm | std's stage gets `loads` only in a fresh round; with units every std file is loaded and the app copy keeps std's value (None in the keyed path). |
| 200 | loads_added | `usize` | C | – | lazy loading. |
| 201 | recursive_union_instances | `usize` | P | – | instance-naming counter (`recursive_unions.rs:217`). |
| 202 | added_defs | `Vec<(u32,u32)>` | A | – | pushed only by `collect_added_in` (`collect/mod.rs:188`), never by `collect_std`. |

### 1b. `units::Summaries` (`crates/velt_sema/src/units.rs:82-105`), all P

All ten are written only when `units_stage == Std` and read by the app's passes; all are plain
`DefId`/`TyId`/`Span`/`LocalId`-keyed data, no `'m`, no `Rc` (checked the defining files).

| field | type | read by the app at |
|---|---|---|
| key_needs | `record_keys::KeyNeeds` (needs, pneeds by DefId) | `record_keys.rs:234` |
| async_summaries | `ownership::AsyncSummaries` (params, rets, hits, insts, parent, news, dispatched, referenced, dyn_owners) | `ownership/local_async/summaries.rs:40-138`, `instances.rs:56` |
| json_needs | `json::JsonNeeds` | `json/mod.rs:119` |
| tuple_summaries | `struct_tuples::Summaries` (3 × `carry::Summary`) | `struct_tuples/mod.rs:333,442` |
| weak_scans | `weak_threads::WeakScans` (bodies, facts) | `weak_threads.rs:200,240` |
| state_facts | `module_state::StateFacts` | `module_state_facts.rs:78` |
| weak_made | `Option<Vec<TyId>>` | `replaced_holders::implicit` |
| undefined_facts | `undefined_args::UndefinedFacts` | `undefined_args.rs:93` |
| lock_summaries | `ownership::LockSummaries` | `ownership/locked/summary.rs:82` |
| module_edges | `linked::ModuleEdges` (module → modules) | `linked.rs:35` |

### Counts

| class | Ctx fields | of which "?" |
|---|---|---|
| P (incl. mixed, `sigq`, and "P or derivable") | 96 | 9 |
| M | 19 | 0 |
| C | 83 | 9 |
| A | 4 | 2 |
| total | 202 | |

Plus the 10 `Summaries` fields (all P).

Fields holding `'m` (7): `modules`, `gen`, `info`, `aliases`, `sigq` (`deferred_sigs`),
`import_targets`, `loads`. Of these only `modules`, `info` and `aliases` are P; `sigq` is P only
for its span set. Fields holding `Rc`/`RefCell`/`OnceCell`/`Arc` (12): `gen` (Rc), `marked_aliases`
(OC), `classes_below` (Rc), `canon_keys` (Rc), `literal_display` (RC), `jsx_providers` (Rc),
`pure_fns` (Arc inside), `source_facts` (Arc), `js_api_modules` (OC), `native_libraries` (OC),
`decorator_scopes` (RC), `export_memo` (RC + Rc). **None of them is P**: every shared/interior
cell is a memo or per-check state, so the saved form needs no `Rc` identity. Owned AST values:
`objects.brands.direct` (`ast::Ident`, re-installed), `closure_groups` (`ast::Expr`, re-installed),
`type_defaults.resolving` (transient).

## 2. Summary of the P fields and the types they need encoded

### Definitions, types, HIR
- `ty` → `TyTable { kinds: Vec<TyKind> }` (+ rebuild the intern map), well-known ids, `inferred_fns`.
  `TyKind` (`hir/mod.rs:85-142`): ints/floats, `Adt`, `Array`, `Map`, `Tuple`, `Option(_, Nothing)`,
  `Result`, `Promise`, `Shared`, `FnPtr{..}`, `Closure`, `Param`, `Dyn`, `Literal(LitValue)`,
  `Symbol`, `Simd`, `Op(TyOp, ..)`.
- `defs` → `hir::Def` (`Fn(FnDef{ body: hir::Body .. })`, `ExternFn`, `Adt`, `Enum`, `Global`,
  `Interface`): the whole HIR (`crates/velt_sema/src/hir/*.rs`, ~2.5k lines of types; no `Rc`, no
  AST, derives Clone/Debug only).
- `info` → `DefInfo` (`defs.rs:207-542`): `FnInfo` (Generics, Bound, ParamSig with
  `default: Option<hir::Expr>`, ThisSig, ThrowSrc, DeclaredThrows, LocalKind, HostImport, CImport,
  DeclaredIntrinsic, Inline marker, plus `source`/`extern_sig` = `'m`), `AdtInfo` (FieldInfo with
  `hir::Expr` default, Redefined, MethodRef, vtable, `decl` = `'m`), `EnumInfo` (VariantInfo, `decl`),
  `IfaceInfo` (IfaceMethod, `decl`), `GlobalInfo` (`init: Option<hir::Expr>`, CTy, CGlobal, `src` = `'m`).
- `def_spans`, `fn_defs`, `fn_shapes`, `fn_values`, `closure_counts`, `nested`, `nested_locals`,
  `enclosing_class`, `static_this`, `static_copies`, `static_copy_of`, `defaults_checked`,
  `field_defaults_checked`, `shaped`, `laid_out`, `default_throws`, `widening_values`,
  `sigq.readonly_params`, `diags`.
- aliases: `aliases` (`AliasInfo`: module, `decl` = `'m`, flags, `outer`), `alias_names`,
  `recursive_unions`, `generic_recursive`, `recursive_union_insts`, `recursive_union_instances`,
  `type_defaults.resolved`, `type_level` (op_sites, op_modules, intersection_parts, converts,
  failed_sites, key_kinds).

### Scopes, exports
- `scopes` (`ModuleScope`: items `Map<String, Item>`, exports, namespaces, namespace_aliases,
  type_only, via_export_type, flags), `prelude`, `prelude_type_keys`, `modules` (metadata: path,
  file, is_std, is_dependency, imports). Export tables are a memo built from AST (see section 3).

### Impls, extensions
- `impls` (`hir::ImplDef`), `extensions` (`Extension`: Generics, target TyId, methods,
  implements with spans), `ext_impls`, `ext_by_iface`, `iface_writes` (incl. `AdapterParts`,
  `ArrayReads`), `iface_adapters` (AdapterKey/Adapter), `dyn_iface_targets`, `object_impls`,
  `hook_adapters`, `fills`, `generic_overrides`, `getter_only_setters`, `abstract_methods`,
  `defined_setters`, `generic_called`, `generic_fns` (incl. `hir::FreshClosure`, `hir::FnWrapped`).

### Objects, layouts, interning
- `objects` (frozen, frozen_defs, hidden, `Layouts` {families: `Family`, redirect, conflicts,
  parsed}, class_values, expansive_cut, record_fills), `anon` (ShapeField keys), `readonly_twins`,
  `any_unknown_views`, `brands`, `brand_keys`, `generic_brands`, `unique_symbols`, `record_views`,
  `field_only`, `field_only_of`, `unions`, `part_ifaces`, `synth_ifaces` (Form keys),
  `union_views` (ViewInfo, FamilyKey, Families), `any` (AnyDefs), `literal_ifaces`,
  `literal_structs`, `generator_values`, `fn_views`, `symbol_keys` (KnownSymbol, SymbolOwner,
  u128 ids), `object_copies` (Copy, Write, Seen, ...), `method_facts`.

### Throws, fn sites
- `fn_sites` (FnTypeSeeds, sites, by_file (FileId keys), flows, written, param_types,
  value_args, solved_anons, `Portable`), `provisional_slots` (ThrowSrc), `promise_forwarders`,
  `primitive_guards`, `cpu_targets` (hir::CpuTarget), `decorator_uses` (hir::Decorator),
  `c_exports` (hir::CExport), `super_calls`, `counted_objects`, `changes_held`, `kept_fns`,
  `kept_consts`, `frozen_reads`, `nothing_slots`, `overload_sets`, `writes_undefined`,
  `lock_free_callbacks`, `module_scripts`, `state_refused`, `held_borrows_used`, `lazy`.

### Summaries
- `unit_sums` (section 1b).

### Encoders or hashers that exist
- `velt_syntax::encode_module`/`decode_module` (`crates/velt_syntax/src/codec/{mod,nodes}.rs`):
  a hand-written, field-by-field compact **AST** encoder (LEB128, spans without file, `FORMAT`
  header, compile-fail on new AST fields, decode never panics). Used by
  `crates/velt_loader/src/std_ast.rs` (StdAstCache, kind `std-ast`, keyed by compiler identity +
  source CRC/hash). Its `Writer`/`Reader` are private to velt_syntax: reusable as a design, or
  as code if made public. It encodes nothing of sema. It can encode the **rewritten** std
  modules (they are `ast::Module` too).
- `crates/velt_loader/src/std_index/codec.rs`: another compact codec (std index), same style.
- `crates/velt_build_cache` (`pack.rs`): the store, CRC-32 integrity, keys by kind.
- `crates/velt_sema/src/interface/{hashing,std_defs,tables}.rs` (`interface::Summary`,
  `std_defs`/`std_def_texts`, `VELT_STD_SUMMARY[_SHOW|_EAGER]`, driver
  `crates/veltc/src/driver/std_summary.rs`): **structural hashers** of a `hir::Program`
  (TyIds by structure, DefIds by name, spans dropped). Not reversible, so no encoder, but (a) they
  destructure every HIR struct without `..` (a ready checklist of what an HIR encoder must cover)
  and (b) they are a ready verify oracle: hash std's defs from a loaded unit vs a freshly checked
  one.
- `crates/velt_sema/src/kept_reload.rs`: `Facts` (Debug-hash of the rewrite facts per module,
  `hash_of(&m.ast)`), and `decl_path`/`decl_at` (an `FnDecl`'s position in its module: item index +
  class/struct/extend member), already used by `kept.rs:466-526` to re-point a cloned context's
  `FnSource::Decl` and `cx.modules[m]` at another module's AST. That is option A's relink, for
  top-level and member functions.
- `program.rs:20-173` (`VELT_SEMA_CLONE_CHECK`): builds the program from a clone and compares
  field by field (`parts`, `differs`); the same harness can compare "fresh std unit" vs "loaded
  std unit".
- No serde/bincode in velt_sema or velt_common; HIR derives only Clone/Debug (and Eq/Hash on
  small types). There is no encoder of any sema type today.

## 3. Where the app's check reads std's AST through the frozen context

Each row: read path (file:line) → what of std's AST it reads → can std's stage compute it?

### Type aliases (the largest dependency)
- `alias_expand.rs:31-70` `expand_alias`/`alias_body`: every app use of a std alias resolves the
  alias's `decl.ty` `TypeExpr` again in a `TyEnv` of the std module (and writes `used`/`expanding`).
  Also `resolve.rs:457` (`decl.ty.span` into `fn_sites.alias_written`). → Partly: a non-generic
  alias could be stored as its `TyId`, a generic one as a `TyId` over `Param`s, but alias
  expansion also drives deferred type-level ops, recursive unions, written-form display, error
  spans and `fn_sites.alias_written`; needs an alias-body IR. Hard.
- `recursive_unions.rs:31,48,60,211` (decl generics/body), `intersection_members.rs:160`
  (`decl.ty.kind`), `type_level/mapped.rs:61-64` (mapped alias body), `body/readonly_arrays.rs:268`,
  `body/expr/fn_casts.rs:253-259` (clones `decl.ty`), `body/expr/simd_call.rs:19-21` (alias name and
  generics), `record_keys.rs:130-143` (`marked_aliases`, **recomputed by the app** after
  `units.rs:61` resets it). → simd/fn_casts/readonly/marked: yes (flags/names per alias, or keep
  `marked_aliases` instead of resetting it, since std's aliases don't change). Type-level/mapped
  and recursive unions: only with the alias IR.
- `alias_expand.rs:158` (`check_unused_aliases`, app's aliases only: `used` flags of std aliases
  were set in std's stage) → not a std read in practice.

### Type-parameter defaults
- `resolve.rs:579,585,737` + `type_defaults.rs:46-61,100` (`with_defaults` reads the AST
  generics' defaults of std classes/interfaces/aliases; memoized in `type_defaults.resolved`). →
  Yes: resolve every std generic's defaults in std's stage (`Generics.defaults` already exists
  in `defs.rs:20`).

### Function declarations (`FnSource`, `extern_sig`)
- `collect/static_this.rs:74` `uses_this` + the copy's body check (`body/driver.rs:449-600`): an
  app class extending a std class gets copies of std's static methods that use `this`, checked
  from std's **body AST**. → Not without a HIR-level copy (substitute `this` in std's checked
  HIR instead of re-checking source). Medium-hard.
- `collect/fill_defaults.rs:61,81,115` (default-parameter positions/expressions of a std method an
  app override fills) → Yes: `ParamSig.default` already holds a `hir::Expr`; store positions.
- `body/predicates.rs:357-372` `predicate_of`: the return annotation `x is T` / `asserts x` of a
  std function, at each app call. → Yes: store `(Option<TyId>, param index, asserts)` in FnInfo.
- `type_queries.rs:259-262,345` (`typeof stdFn`: params' rest/optional/default flags,
  `declared_fn_type`), `typeof_generic.rs:52,102`, `collect/sigs.rs::extern_sig` re-reading an
  extern's AST signature (`type_queries.rs:262`). → Yes: per-param flags and "declared
  function type" computed in std's stage.
- `generated/mod.rs:281,320` (generator signature of a std function with a source generator;
  the app's generated phase serves std's demands too) → Yes: a flag/TyId.
- `decorators/known.rs:75`, `decorators/mod.rs:102,117` (`extern_sig.is_decorator`, decorator
  signature) — app decorators naming std decorator functions → Yes: store the decorator's sig
  facts.
- `defs.rs:338,358` (`has_this_param`, `returns_this` read `decl.sig.this_param` / ret) → Yes:
  flags.
- `collect/ret_infer.rs:19-20` (`returns_value` of a source; std's results are inferred
  already) → not reached for std defs in the app ?.
- `body/gated.rs:68`, `body/literal_hosts.rs:46`, `body/unsafe_code.rs:148`,
  `collect/operators.rs:174`, `throws/conventions.rs:22,156` (`source.is_none()` checks) →
  Yes (flag "has source").

### Type declarations (`AdtInfo/EnumInfo/IfaceInfo.decl`)
- Flags only (`decl.is_some()`, `c_struct`, `c_type`, `is_abstract`, `ctor_visibility`):
  `finalize.rs:269-270`, `copy_types.rs:26,73,83`, `iface_writes.rs:219,267`,
  `body/expr/access.rs:118`, `body/expr/construct.rs:61,73`, `body/expr/proxy.rs:302`,
  `collect/c_struct.rs:22,165`, `collect/c_sig.rs:412`, `collect/extend_shadow.rs:39`,
  `collect/classes.rs:26,155` (app subclass vs std base), `struct_tuples/mod.rs:58,151`,
  `collect/field_only.rs:26`, `collect/iface_extends.rs:22,116-117`, `body/defaults.rs:311`.
  → Yes: a few booleans per def.
- Counts/names: `utility_types.rs:331` (`decl.fields.len()`), `collect/field_only.rs:136`,
  `collect/iface_extends.rs:117` (own fields of a std interface an app interface extends),
  `enum_keys.rs:18` (`keyof typeof StdEnum`: member names), `body/expr/ctor_objects.rs:265-273`
  (statics of a std class as an object), `body/expr/regex_args.rs:210`,
  `body/expr/system_args.rs:204`, `body/expr/ctor_values.rs:143`, `creation_notes.rs:14`
  (diagnostic note), `collect/classes.rs:301-419` (`decl.methods` of the **base** when checking an
  app subclass's overrides/setters: `classes.rs:354-359,412-419` look at the base decl's
  getters/setters) → Yes: own-field counts, member lists with getter/setter/static flags, enum
  member names.
- `body/driver.rs:85-90` (field defaults of a std class from its decl when first used;
  guarded by `field_defaults_checked`, which std's stage filled) → not reached if that set is
  saved.

### Global constants (`GlobalSrc`)
- `body/expr/names.rs:429-433` (untyped integer `const` of std: its initializer's literal),
  `body/expr/widening_consts.rs:206,262-265`, `body/literal_patterns.rs:396`,
  `body/expr/generic_conversions.rs:194`, `symbols.rs:152,191,213,341` (symbol names from a const's
  initializer), `unique_symbols.rs:29`, `decorators/consts.rs:234` and
  `decorators/suffixes.rs:176` (constant evaluation of decorator arguments),
  `type_queries.rs:140` (`typeof stdConst`), `body/expr/system_args.rs:170`,
  `throws/fn_flows.rs:360` (annotation span), `body/unsafe_code.rs:108`, `body/expr/names.rs:442,489`
  (`c_global`). → Yes: a "constant summary" per std global (literal value if any, symbol
  description, has-annotation, c_global flags).

### Modules (`modules[m].ast`)
- Export tables `collect/exports.rs:85-140,255-281,512` and `collect/export_walk.rs:40,178`:
  built lazily per module from the module's export items/lists; `Memo::changed()` (from
  `exports.rs:69` namespace_bound, `collect/default_snapshots.rs:36`, `std_load.rs:288`) clears
  **all** tables, std's included, so the app rebuilds std's tables from std's AST. → Yes: build
  every std module's table in std's stage and keep std's entries across `changed()` (std's exports
  cannot depend on the app).
- `script_tdz.rs:22-36`: the app stage runs `script_tdz::check` over **every** module, std's
  included (std's stage never runs it). → Yes: run it for std in std's stage, for app modules
  in the app's.
- `module_state.rs:224-232` `evaluates(module, spec)` (import kinds, for module evaluation order,
  `module_state::order` each round, `lib.rs:727`) → Yes: per std module, the evaluated imports.
- `js_api.rs:48-60,117` (`js_api_modules`, prelude re-exports) → Yes (keep the OnceCell's value).
- `decorators/scope.rs:34,220-226` (scopes below module level for decorators of a module) →
  only for modules whose decorators are checked: app's; std's done in std's stage ?.
- `collect/nested.rs:49`, `collect/declare.rs:32-87` (declare): app modules only (`declare_added`).
- `generated/mod.rs:291-292` (any module declares a generator) → Yes (flag per module).
- `symbols.rs:317`, `linked.rs:51`, `generated/phase.rs:168` etc. read only `path`/`file`.
- `import_targets` reads `modules[..].imports` (`collect/exports.rs:401-413`) — metadata, not AST.

### Not through the frozen context
- The source rewrites (`lib.rs:279-339`: module_chunks ... nested_closures, `augment_scope`,
  `interface_merge`, `namespaces`, `overloads`, `call_sigs`) read and rewrite **all** modules
  before any context exists. They need std's (parsed) AST in every check, whichever option is
  chosen, until they too are made per unit.

## 4. Options

### (A) Save std's unit; re-link `'m` references by position on load

What it takes:
1. An encoder/decoder for the P set (≈100 fields + Summaries) in the style of
   `velt_syntax::codec`: field-by-field, the `Ctx` destructured without `..` so a new field fails
   to compile until it is classified (save, or reset with a reason). Biggest parts: the HIR
   (`hir::Def`, `Body`, `Expr`, ~2.5k lines of types), `TyKind`, `DefInfo`, and ~80 side tables
   of `DefId`/`TyId`/`Span`/`String` maps. M and C fields are reset to `Default` on load (as a
   fresh `Ctx::over` would have them, then `add_app` + `Found::install` as today).
2. Re-linking: `'m` pointers appear only in `modules`, `info` (`FnSource::Decl`,
   `FnSource::Default(&FnSig,&Block)`, `extern_sig`, `AdtInfo/EnumInfo/IfaceInfo.decl`,
   `GlobalSrc.{ann,init,c_global}`) and `aliases[..].decl` (`import_targets` is recomputed;
   `sigq.deferred_sigs` is empty after std). Encode each as a position in its module: item index
   + member path (`kept_reload::decl_path` already does item + class/struct/extend member), and
   for things inside bodies (nested items, `FnSource::Default` of arrows/closures/defaults) a path
   of statement/expression indices, or a `(node kind, span) → node` index built by walking the
   module once on load.
3. Which modules to re-link into: the unit borrows the **rewritten** std modules
   (`std_unit::copies` copies the post-rewrite trees), not the loader's parsed ones. Simplest:
   keep running the rewrites over all modules in the fresh process (they run today anyway), and
   re-link into this check's rewritten std modules, guarded by the unit's key (`Found::std_key`).
   Alternative: also store the rewritten std modules with `encode_module`.
4. Key: compiler identity + std sources' hashes (the loader's key) + `Found::std_key`'s parts.
5. A verify mode (`VELT_STD_UNIT=verify`): check with a freshly computed unit and with the loaded
   one, compare programs and diagnostics with `program.rs`'s `differs`, and std defs with
   `interface::std_defs`.

Estimate: encoder ≈ 3-5k lines (HIR and types ≈ half), re-link + load/key/store ≈ 0.5-1k, verify
mode small (existing harnesses). Roughly 2-4 weeks for one agent, most of it the HIR encoder,
which phase 3 needs anyway ("the HIR and interface encoding").

Risks:
- **FileIds in spans.** Hundreds of thousands of spans (HIR, `fn_shapes`, `default_throws`,
  `super_calls`, `widening_values`, `fn_sites.*`, `type_level.op_sites`, span sets) carry the
  FileId std's files had in the process that saved them. A fresh process may number std files
  differently (load order, app files first). Encode spans as (std file ordinal, lo, hi) and
  map back on load, rehashing every span-keyed map; or make std's FileIds fixed. `std_key` also
  hashes `(file.0, lo, hi)` (`std_unit.rs:117-128`), so today's key is not stable across processes.
- **Position re-link targets.** Rewrites synthesize nodes (generic_arrows lifts, overload
  signature functions `name#k`, call_sigs, nested_closures, interface_merge): span lookups can
  collide; positions must be taken in the rewritten tree and the rewrites must be deterministic
  for the key's inputs. Deep `FnSource::Default` pointers (closures inside bodies) need more than
  `decl_path`.
- **Hash-map iteration order.** Many side tables are Fx maps/sets iterated by passes (e.g.
  `cx.fills.iter()` then sorted, but not everywhere). A decoded map can iterate in another
  order than the original (it depends on insertion history), so DefId allocation or diagnostic
  order could differ between a loaded and a fresh unit. The verify mode would show it; encode in
  sorted order to keep loads deterministic among themselves.
- **Field drift.** ~200 fields, changing in most sema PRs. Without the destructuring discipline
  and the verify mode, a missed field gives plausible but wrong results (exactly the failure mode
  the in-memory clone already guards with `VELT_SEMA_CLONE_CHECK`).
- **"?" fields.** About 20 fields above are uncertain (C vs P). Saving them all costs little
  (they are small `DefId`/`Span` sets), so err toward P and let the verify mode find any
  that can be dropped.
- **State outside the classified sets**: `stack_base` (an address on the old thread) must be
  reset; the type intern map rebuilt; `DefTable` frozen prefix re-set; `TyTable` invariants.
- **Size/time.** All of std's HIR must decode faster than std's check takes; not measured here.
- The HIR is a maintainer-owned contract: the encoder must live in sema next to it and fail to
  build on HIR changes (that is the point), so every HIR change also touches the encoder.

### (B) The app's check needs no std AST at all

What it takes: everything in (A) except re-linking, plus replacing every read path in section 3:
~15 kinds of declaration flags/counts (easy), predicates, typeof facts, constant summaries for
globals, export tables built in std's stage and kept across `Memo::changed`, `script_tdz` and
module-order facts moved to std's stage (moderate), and two hard ones: **alias expansion** (an IR
for alias bodies that the deferred type-level ops, recursive unions, defaults and display can work
from) and **static-method copies** for app subclasses (copy and re-type std's checked HIR instead
of re-checking its source). Each change alters checker behaviour, so each needs its own
regression tests and the identity oracle.

Estimate: (A)'s encoder (2-3 weeks) plus 3-6 weeks for the read paths, the alias IR alone being
open-ended. And it buys little now: the source rewrites (section 3, last block) still read every
std module's parsed AST in every check, and `velt check` by units loads all of std anyway, so std's
AST is in memory regardless until the rewrites become per unit too.

Risks: the alias IR (semantic drift from TypeScript in edge cases: defaults, recursive and
conditional aliases, error spans); HIR copies of static methods (`this`/`super` substitution,
closures inside); larger behaviour surface than (A), so more chances of program-visible
differences; the work is not done until the last read path is gone.

## Recommendation

(A) now, built so that it moves toward (B):
- Write the HIR/types/DefInfo encoder (needed by both, and by phase 3).
- Re-link by position into this check's rewritten std modules, under the existing key made
  process-stable (FileIds by std ordinal), with a verify mode from day one.
- Then convert the cheap read paths of section 3 one by one (flags, predicates, constant
  summaries, export tables, `script_tdz`, `marked_aliases`/`js_api_modules` kept instead of
  reset). Each removes re-link entries. Aliases and static copies go last, together with making
  the source rewrites per unit, which is when dropping std's AST from the fresh process starts
  to pay.
