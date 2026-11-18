# #2943 M2: what the app's stage reads of std's AST

This is a read-only audit of `crates/velt_sema/src` in `/Users/andii/dev/velt/branches/d1-open-pr` (branch `units-passes`).

- The functions, types and aliases parts give line numbers at e4ca2e67c.
- The modules part gives line numbers at acddf0f0c, which another session committed during the audit. Of the files cited, only `ownership/local_async/targets.rs` changed between the two, and `units.rs` became `units/mod.rs`.

The question: in `VELT_UNITS=2`, which reads of std's AST does the app's stage make (that is, the stage that runs on a copy of std's frozen `Ctx`)? For each read: what it needs, and what could replace it.

## Summary

### Kinds of read

| Kind | Sites that read std in the app's stage | Recommended replacement |
|---|---|---|
| Function signature flags: `rest`, `is_unsafe`, `returns this`, JS `length`, `this` param, `ret` written `readonly` | ~10 (every app call of a std function goes through `has_rest` and `unsafe_fn`) | Owned bools on `FnInfo`, set in `fill_sig` |
| The first statement of a std body, read on every free-function call (`gated.rs:62 fn_target`) | 1 | `cx.cpu_targets` (already filled while std's bodies are checked) |
| A std method body walked for `this` (`call_apply.rs:314`) | 1 | Owned `body_uses_this` |
| Per-parameter default *literal* or default shape: method values, generator values, fill slots for app overrides, `typeof` flags | 5 | Owned per-param flags plus a literal key, or derived from the checked `ParamSig.default`. **The parameter defaults themselves need no AST**: they are memoized (`defaults_checked`) and placed from the checked `hir::Expr` |
| Signature re-resolved in the std module: `typeof stdFn` (`declared_fn_type`), `typeof stdGenericFn` (`typeof_generic.rs`), std type predicates (`predicates.rs:389`) | 3 | Use the already-resolved `FnInfo` types; memoize the generic fn type and the predicate `(asserts, index, TyId)` in std's stage. Otherwise keep the `FnSig` AST of that declaration |
| Generated functions: `generator_of` and `has_generated_fns` (the latter walks every module) | 2 | Owned `GeneratorSpec` (~13 in std), plus a bool from `unit_sums.generated_fns` |
| **Type aliases**, re-resolved from the AST at every use (`alias_body`) and when a deferred `TyOp::Alias` is reduced | ~12 | Non-generic (~183/199): memoize the expansion (`TyId`) in std's stage. Generic (16): keep that declaration's AST (owned `ast::TypeAlias`, like `type_level::inline::Form`). Names and flags owned |
| `check_unused_aliases` walks every std alias std did not use, because std's stage returns before it (lib.rs:609) | 1 | Run it at the end of std's stage; from `Added.aliases` on in the app's stage |
| Decorators: a std `declare decorator function` used by the app (`is_decorator`, target bits, param `rest` and **default evaluated as a constant in the std module**) | 3, + consts.rs:234 | Owned decorator record (4 in std): `(bits, repeatable)` and per-param `Option<ConstTree>` |
| Type declaration flags: declared, `c_struct`, `c_type`, `is_abstract`, `ctor_visibility` | ~20 (`is_copy`, `finalize`, `struct_tuples`, `new` ...) | Owned `DeclFacts` on `AdtInfo`/`IfaceInfo`/`EnumInfo`. `declared` must stay true for std types (`abstracts.rs:68` stops walking at `decl == None`) |
| Own member counts and keys of std bases/parents | 6 | Owned counts, plus `override` and abstract key sets |
| Which type parameters have defaults (`with_defaults(&decl.generics)`) | 4 | `Vec<bool>`; the values are memoized in `type_defaults.resolved` |
| Real content of std type declarations: `field_only.rs:89` (resolves std parents' `extends`), `regex_args.rs:210` (field default + ctor body), `system_args.rs:204` (field's written type), `ctor_objects.rs:265` (statics), `enum_keys.rs:18` (discriminant exprs) | 5 | `cx.field_only` map; owned regex shape; per-field `written_readonly`; `has_shown_statics`; `VariantInfo` |
| `GlobalSrc.ann/init/c_global` of std constants that app code names | 15 | Owned facts per std global: `query_ty`, `known_symbol`, `const_text`, `int_lit`, `case_lit`, `widening`, `alias_of`, `ann_readonly`, `ann_span`, `c_global: Option<bool>`. `as const` tuples keep that init's AST |
| Module-AST walks over std in the app's stage: `script_tdz::check`, `module_scripts::declare`, `module_state::evaluates` imports, `exports::imports_of` (export tables std never built, or cleared by `Memo::changed()`), `any_exported_namespace`, `js_api_modules`, `has_generated_fns`, `extern_sig` | ~9 | Run them in std's stage or start at `app_base`; per-module owned import records and export tables built for every std module in std's stage |
| `sigq.deferred_sigs` | 0 | None: `collect_std` drains it |
| Pre-`Ctx` source rewrites that walk every std module on every check (module_chunks, call_sigs, module_destructure, namespaces, interface_merge, generic_arrows, overloads, recursive_aliases, global_this, nested_closures) | 9 passes | Run them over std once and keep the results in the unit. `interface_merge` (the app augments std) and `global_this` are the hard ones. std's unit key holds app-scan flags (rewrite_facts.rs:103) |

`SourceModule` metadata (`path`, `file`, `is_std`, `imports`) is read for std all over the place, but it is not AST. Name resolution in a std module reads only `cx.scopes`/`cx.nested`, so carry those in the unit.

### Riskiest

1. **Generic aliases and `TyOp::Alias` reduction** (`type_level/mod.rs:220` → `alias_body`). A std generic alias deferred in a std signature is re-resolved from its AST in the std module's scope at the app's type arguments. The same holds for std's inline forms (`type_level::inline::Form`, which already own an `ast::TypeExpr`). So the unit must carry those AST fragments and std's scopes, not just types.
2. **`names.rs:429`**: an untyped std int const's initializer AST is re-checked inside the app function.
3. **`field_only.rs:89`** resolves a std parent interface's `extends` types again, and **`typeof_generic.rs:102` / `predicates.rs:389`** resolve std signatures again.
4. **`exports::imports_of`** together with **`Memo::changed()`**: export tables of std modules can be rebuilt from std's ASTs whenever the memo is cleared.
5. **Pre-`Ctx` rewrites**: today they run over std's ASTs on every check. M2 cannot skip loading std's ASTs until they are split per unit.

### Side findings (not M2)

- `static_this::copy_statics` makes no copy for an app subclass of a std class whose static method uses `this`.
- `default_snapshots::declare` is not called from `declare_added`.
- `module_scripts::declare` may give a std module a second slot.

---

# M2 audit, part: functions (`FnSource`, `extern_sig`), aliases, decorators

Branch `units-passes` (e4ca2e67c), `crates/velt_sema/src`. Read-only.

Std counts (rough grep of the 839 non-test `.velt` files): 199 `type` aliases, 16 of them generic;
4 `declare decorator function`s; about 13 `declare generated(...)` functions; 31 `targetFeature(...)`
gates; 8 type predicates (`x is T`); 8 methods written `: this`; about 400 generic free functions.

Rules that keep most sites quiet (they must survive the save/load):
- `defaults_checked` (body/defaults.rs:35) and `field_defaults_checked` (body/driver.rs:85) are
  filled for every std def by std's `check_bodies_from(0)`. `param_defaults`/`field_defaults`
  return before reading the AST. Calls place the checked `ParamSig.default` (`hir::Expr`, owned:
  body/expr/default_args.rs:20 `placed_default`, plus `default_throws` by span). **So defaults
  need no AST.**
- `ensure_body` returns for `state != Unchecked`. Every std body is `Done` after std's stage
  (lazy is off in units mode). `literal_hosts::find`, `static_this::copy_statics`,
  `decorators::check`, `collect::*` and `resolve_def_sigs` all start at `from` (app defs).

## Functions: `FnSource` / `extern_sig` / `fn_sig_ast`

| Site | Std def in the app's stage? | What it reads | Replacement |
|---|---|---|---|
| **body/defaults.rs:27 `has_rest`** (called from call.rs:217 `fn_callable`, names.rs:678, generic_values.rs:332/518, callback_sig.rs:240, jsx/automatic.rs:95, virtual_defaults.rs:55, construct.rs:131, type_queries.rs:305) | **Yes**: every call of a std function or method from app code (`fn_callable`) | `sig.params.last().rest` | owned `FnInfo.rest: bool` (set in `fill_sig`) |
| **body/unsafe_code.rs:144 `unsafe_fn`** (unsafe_code.rs:48/68/128 via `check_unsafe_call` in `fn_callable`; any.rs:345; collect/unsafe_methods.rs:22 overriding a std base) | **Yes**: every call of a std function | `sig.is_unsafe` | owned `FnInfo.is_unsafe: bool` |
| **body/gated.rs:62 `fn_target`** (gated.rs:119 `check_gated_call` on every free-function call in call.rs:352; gated.rs:142 `gated_value`) | **Yes**: every call of a std free function (the `is_std` test passes for all of std), and a std function used as a value | **the first statement of the body** (`targetFeature("…")` and what its callee binds to) | `cx.cpu_targets` already maps each gated std fn to its target (gated.rs:113, written while std's body is checked). For `d < unit_base`, read that map. No AST needed |
| defs.rs:334 `has_this_param` (this_param.rs:22 `takes_this` on every call, call.rs:350; driver.rs:658) | Rarely: it short-circuits on `kind ∈ {Free, Closure}` and `this.is_some()` first, so only std free functions with a `this` parameter reach the AST | `sig.this_param.is_some()` | owned bool `FnInfo.this_param` |
| **defs.rs:353 `returns_this`** (body/expr/this_type.rs:36/69 on method calls; collect/classes.rs:554/569 for an app override of a std method) | **Yes**: a call of a std method, and an app class overriding one | whether the written result is the bare type `this` | owned bool `FnInfo.written_this` (8 such methods in std) |
| body/expr/this_param.rs:139 `this_fn_value` | Yes, but only for a std free function with a `this` parameter (after `takes_this`) | the whole `FnDecl`: generics, async, generator, params' patterns/rest; then it builds a forwarding declaration from it (`forwarding_decl`) | per-declaration AST for those few functions (1 in std) |
| **body/predicates.rs:359 `predicate_of`** | **Yes**: `if (isX(v))` where `isX` is a std type predicate | the written result `v is T` / `asserts v`: predicate kind, the param's index, generics empty, **and it resolves `T` in the std module again** (predicates.rs:389) | owned `FnInfo.predicate: Option<(asserts, param index, Option<TyId>)>`, resolved in std's stage (8 in std) |
| **body/expr/fn_shape.rs:74/88 `declared_length` / `declared_fn_member`** | **Yes**: `stdFn.length`, and the shape of a std function passed as a value (fn_shape.rs:149/174) | `decl_length(&sig.params)`, the JS `length` (params before the first optional/default/rest) | owned `FnInfo.js_length: u32` |
| **body/expr/method_value.rs:396 `literal_defaults`** | **Yes**: a std method taken as a value (`xs.map(obj.m)`, `const f = s.trim`) | per param, its default **if it is a literal** (`literal_defaults(&params)`) | owned per-param `Option<literal>`; or derive it from `ParamSig.default` (a checked `hir::Expr`: a literal is `H::Lit`) |
| body/expr/method_value.rs:483 `param_names` | Yes (same path, for the wrapper's names) | param names with `...` for rest | `ParamSig.name` + `FnInfo.rest` |
| **body/expr/gen_value.rs:42/108 `generator_value`** | **Yes**: a std generated function read as a value | the function's name ident; per param `rest`, `optional`, whether the default is the literal `undefined` | owned per-param flags (`rest`, `optional`, `default_is_undefined`) + `FnInfo.name`; or `ParamSig.default` |
| body/expr/system_args.rs:175 | Yes: an argument of a system/host call that is a call of a std function | whether the written result type is `readonly …` (`is_readonly_written`) | owned bool `FnInfo.ret_written_readonly` |
| **type_queries.rs:259 `query_fn`** (`typeof stdFn` in an app annotation) | **Yes** | per param `rest`, `optional`, has a default; for a `declare` function it calls `collect::sigs::extern_sig`, which **walks the std module's `ast.items`** to find the `ExternFn` by span (sigs.rs:94) | owned per-param flags (`rest`, `optional`, `has_default`) |
| type_queries.rs:345 `declared_fn_type` | Yes, but only while the app is collected (`bodies_ok` is false in `collect_app`, collect/mod.rs:178) and only for `typeof stdFn` in an app field type | the whole signature; it **resolves the param and result types again** in the std module | for a frozen def, use `FnInfo.params[i].ty` / `ret`, which are resolved already (plus the flags above) |
| **typeof_generic.rs:52/102 `generic_query_type` / `declared_sig_type`** | **Yes**: `typeof stdGenericFn` or `typeof stdOverloadedFn` in an app annotation | `sig.generics` non-empty; then it builds a function-type AST from the signature (`call_sigs::fn_type`) and **resolves it in the std module** | per-declaration `FnSig` (rare; about 400 generic std functions could be named), or memoize the resulting generic `TyId` per def when std's stage first computes it. The type built does not depend on the use (the span is only for messages) |
| collect/fill_defaults.rs:61/81/115 `fillable` / `written_default` / `trailing_counts` (from `fill_positions` in `collect/classes.rs`, and `build_fill_bodies(cx, from)`) | **Yes**: an app class overriding a std method and giving a parameter another default; `def` is the introducing (std) method | per param: has default, optional, rest, and **the written default expression**, compared with `same_literal` against the override's | owned per-param `default_literal: Option<LiteralKey>` + flags. `ParamSig.default` (checked) could serve for `same_literal` if it compares literals only |
| **collect/static_this.rs:66 `uses_this`** via body/expr/call_apply.rs:314 `method_of_reads_this` | **Yes**: `f.call(...)` / `apply` / `bind` analysis on a method of a std type | **the whole method body**, walked for `this`/`super` | owned bool `FnInfo.body_uses_this` |
| collect/static_this.rs:42 (in `copy_statics`) | No: only app classes and their statics (`from..`). **Side finding:** for an app class extending a std class whose static method uses `this`, no copy is made for the app subclass (`users` holds only app classes in the app's stage). This is a units-mode semantics gap, separate from M2 |
| collect/operators.rs:174 `shape_error` | No: app operator methods (`operators::check(cx, from)`) | params' flags | - |
| collect/ret_infer.rs:19, collect/sigs.rs:44/308/482/537/688 | No: app items (`resolve_def_sigs(items)`) | signatures and bodies | - |
| **generated/mod.rs:293 `generator_of`** → generated/mod.rs:602 `gen_function` | **Yes**: the app's stage runs `generated::phase` for std's demands too (lib.rs:665), and `gen_function` is called for every generated function the program reaches, std's included (`unit_sums.generated_fns`) | `sig.generator` (`GeneratorSpec { spec: String, span }`) | owned `FnInfo.generator: Option<GeneratorSpec>` (clone it into the frozen unit; about 13 in std). `generated_fns()` already takes std's set from `unit_sums` |
| **generated/mod.rs:303 `has_generated_fns`** (phase.rs:73, mod.rs:351) | **Yes**: every round of the app's stage | **walks every module's `ast.items`, std's included**, looking for a function with a generator | owned bool from std's stage (`unit_sums.generated_fns` non-empty) OR'd with the app modules' walk |
| generated/mod.rs:334 `generated_placeholder` | No: called from `check_fn` (an app body being checked) | - | - |
| body/driver.rs:449/528/567/600/687 (`ensure_body` → `check_fn`) | No: std bodies are `Done` | the body | - |
| body/expr/overload_call.rs:36 `signature_body` | No: `body_def` is the app body being checked | - | - |
| body/recursion.rs:148 (message text for a recursive inferred result) | Only for a def whose body is being checked (app) | written param types for the message | - |
| body/recheck.rs:507, kept.rs:469/525, kept_reload.rs:99 | No: kept checks only (`kept::wanted(input) && order.is_none()`, lib.rs:534) | - | - |
| body/literal_hosts.rs:46 `find` | No: `from..` | - | - |
| decorators/known.rs:75 `intrinsics(cx, from)` | No: app defs (`@intrinsic` is std-only) | - | - |
| ide/* (`defref.rs:239`, `snapshot.rs:216`, `effects.rs:51`) | IDE only, not units mode today. If the IDE ever runs by units, go-to-definition and hover on std need spans and signatures | - | - |

## Aliases (`AliasInfo.decl: &'m ast::TypeAlias`)

An alias is **re-resolved from its AST at every use**: `expand_alias` → `alias_body` resolves
`decl.ty` in the alias's module with the use's arguments (alias_expand.rs:83). Only non-generic
recursive unions are memoized (`recursive_unions`). So every std alias the app names needs its
body.

| Site | Std alias in the app's stage? | What it reads | Replacement |
|---|---|---|---|
| **resolve.rs:457 `item_type_args`** → **alias_expand.rs:31 `expand_alias`** → **alias_expand.rs:62 `alias_body`** | **Yes**: every app annotation naming a std alias | name, name span, generic names, `with_defaults(&decl.generics)` (memoized values; reads only which params have defaults), and **the body type expression**, resolved in the std module's scope; also `decl.ty.span` for `fn_sites.alias_written` | **non-generic, `outer` empty (about 183 of 199): memoize the expansion** (a `TyId`; resolve every std alias once in std's stage, which also covers the unused check below). **Generic (16): per-declaration AST** (an owned `ast::TypeAlias` clone in the frozen unit, as `type_level::inline::Form` already holds an owned `ast::TypeExpr`), plus name and has-default flags owned. The side effects `alias_homes` / `alias_names` are recorded on the first expansion in std's stage |
| **type_level/mod.rs:220 `reduce_op` for `TyOp::Alias`** | **Yes**: a std generic alias deferred in a std signature (`TyOp::Alias(a)` inside a std function's type) and reduced when the app instantiates it | name span (fallback site), then `alias_body` (the body AST) | the same per-declaration AST for the generic aliases. This is the riskiest alias path: it needs the AST *and* name resolution in the std module (`cx.scopes[m]`) at the app's type arguments |
| type_level/inline.rs:75 `canonical_alias` | Yes (from `deferred_alias`), memoized in `type_level.alias_canon` | generic names and the body (`closed` check, canonical text) | memo seeded in std's stage for every std generic alias, or the per-declaration AST |
| type_level/inline.rs:214 `inline_op` | Yes: scans **every** alias with a non-empty `outer` (aliases declared inside generic bodies) | `decl.name.name` | owned name on `AliasInfo` |
| type_level/inline.rs:48 `display_op` | Yes (messages and `display` of a deferred std alias) | `decl.name.name` | owned name |
| recursive_unions.rs:31/48/60/211 (`recursive_union_ref`, `is_generic_recursive`, `recursive_union`, `new_recursive_union`) | Yes, from `expand_alias`. Non-generic: memo hit after reading `decl.generics.is_empty()`. Generic recursive (`SlotContent<T>`-like): a new instance per argument list | generics, body is a union naming itself, members (`holds_expanding`), name/span | owned `generic: bool` + `generic_recursive` memo seeded in std's stage; generic recursive ones need the per-declaration AST |
| intersection_members.rs:160 `string_bound` | Yes: a bound that is a deferred std alias | whether the body is an intersection with `string` | owned bool per alias |
| body/expr/simd_call.rs:19 `simd_type_named` | Yes: `u8x16` etc. imported from `velt/simd` | generics empty, name | owned name + generic count |
| type_level/mapped.rs:61 `written_keys` | Yes: `{ [K in StdAlias]: … }` in app code with a non-generic std alias | **walks the alias body** for its written keys | owned key list per non-generic alias (or the per-declaration AST) |
| collect/c_sig.rs:339 | Yes: an app `declare c` signature naming `StrBytes` and similar from `velt/ffi` | `decl.name.name`, module path | owned name (path is `SourceModule` metadata) |
| **alias_expand.rs:154 `check_unused_aliases`** (lib.rs:614) | **Yes, for every std alias std itself never used**: std's stage returns before this call (lib.rs:609 `Stage::Std` → `return None`), so the app's stage resolves them all from their AST | whole declaration | **run it at the end of std's stage for std's aliases**, and in the app's stage only from the app's first alias (`Added.aliases`). Cheap fix, and it removes the largest alias read |
| type_defaults.rs:60 `check_from` | No: `skip(from.aliases)` | - | - |
| record_keys.rs:135 `marked_aliases` | No: filters `!is_std` | - | - |
| body/expr/fn_casts.rs:253 | No: requires `alias.module == self.module` (the app module being checked) | - | - |
| intrinsic_decls.rs:256 | No: `@intrinsic` declarations are std's (std's stage) | - | - |
| ide/display.rs:46, snapshot.rs:249, defref.rs:108 | IDE only | names, spans | - |

## Decorators (app code using a std `declare decorator function`)

`decorators::check(cx, from.defs)` walks only app defs. Each use resolves to the decorator's
def, and the following read the std declaration:

| Site | Std def in the app's stage? | What it reads | Replacement |
|---|---|---|---|
| **decorators/mod.rs:101 `decl_of`** (from `resolve` :376, `uses` :459, `known::written_def`) | **Yes**: every app use of a std decorator | `extern_sig.is_decorator` | owned `FnInfo.is_decorator` |
| decorators/mod.rs:468 (in `uses`) | Yes | the written result type → targets bits and repeatable (`targets_of(sig.ret)`) | owned `(u8, bool)` per decorator |
| **decorators/mod.rs:518 `arguments`** | Yes | per param: `rest`; **the default expression, evaluated as a constant in the std module** (`consts::value_of(cx, decl_module, default)`) | owned per-param `Option<ConstTree>`: std's `check_declaration` (mod.rs:263) already evaluates each default; keep the value |
| decorators/consts.rs:234 `global_value` | Yes, if an app decorator argument names a std `const` | **the std global's initializer AST** (`g.src.init`), folded | owned folded `ConstTree` per std const that folds (or `GlobalInfo.init`, the checked HIR, if `fold` can work on HIR) |
| decorators/scope.rs:37 `shadow_of` | No: `m` is the module where the decorator is written (app) | - | - |

4 decorators in std; the owned record is tiny.

---

# M2 audit, part: `AdtInfo.decl` / `EnumInfo.decl` / `IfaceInfo.decl` reads in the app's stage

Branch `units-passes` (e4ca2e67c), `crates/velt_sema/src`. Read-only audit.

Std counts (non-test and test `.velt` files under `std/`, rough grep): 286 classes, 0 abstract
classes, 0 abstract members, 88 structs, 0 `declare c struct`, 1 `declare c type` (`FILE`, in
`std/ffi/ffi.test.velt` only), 246 interfaces (147 with `extends`), 2 enums, 5
private/protected constructors, 26 class/struct/interface declarations with type-parameter defaults.

Out of scope, checked: `throws/groups.rs` `b.decl` is `DeclaredThrows` (`throws/groups.rs:41`, a
`TyId` + `Span`), not an AST borrow. The same goes for `forward_consts`, `literal_locals`,
`ownership/locked/later.rs` and `defined_accessors` (their `.decl` fields are local indexes and spans).

Notation: "flag" means a bool or small enum copied out of the `ast::TypeDecl` in std's stage. A
proposed `DeclFacts` (owned, on `AdtInfo`/`IfaceInfo`/`EnumInfo` in place of `decl`) would hold:
`declared: bool`, `c_struct`, `c_type`, `is_abstract`, `ctor_visibility`, `own_field_count`,
`own_method_keys` (or a count, since `IfaceInfo.methods`/`fields` list own members first), and
`abstract_keys: Vec<String>`. In the table, "-> DeclFacts.x" means the site can read that field.

## Collection (`collect_app`, `from` = first app def; `items` = app items)

| Site | Std type in the app's stage? | What it reads | Replacement |
|---|---|---|---|
| collect/classes.rs:26 `check_classes` filter | No: `from..` app defs only | `decl.is_some()` | DeclFacts.declared |
| collect/classes.rs:74 `overridden_methods` | No: `adts` are app classes | own methods (`is_override`, names) | (app AST) |
| **collect/classes.rs:155 `introducer`** | **Yes**: walks `base_of_def` up the chain, so an app class extending a std class reads the std base's (and its ancestors') `decl.methods` | for method key `name`: whether the class declares it with `override` (method name, `is_setter`, `is_override`) | owned set of `override` method keys per class (`overridden_keys: FxHashSet<String>`); in practice it only needs to answer "is the instance method `name` of `c` an override?" |
| collect/classes.rs:301 `own_methods` | No: `d` comes from the order loop, which skips `is_frozen(d)` (classes.rs:37) | own methods | (app AST) |
| collect/classes.rs:354 `shadowed_setter` | No: `d` is the app class | own setters | (app AST) |
| collect/classes.rs:368 `getter_only_overrides` | No: `d` is the app class; it reaches the base only through `lookup_method` (owned) | own methods | (app AST) |
| collect/classes.rs:412 `hidden_getter` | No: `d` is the app class | own getters | (app AST) |
| collect/classes.rs:619 `inherit_ctor` | No: frozen classes are skipped | `decl.is_some()` | DeclFacts.declared |
| collect/abstracts.rs:15 `record` | No: app `adts` | abstract methods | (app AST) |
| **collect/abstracts.rs:68 `check_implemented`** | **Yes**: walks every base class of a concrete app class, std ones included. A base without `decl` *breaks* the walk, so `decl: None` is not a safe stand-in | the base's `decl.methods.filter(is_abstract)`: key and name of each abstract member | DeclFacts.declared + `abstract_keys` (name, key). Std has 0 abstract classes today, so for std this is always "declared, no abstract members" |
| collect/abstracts.rs:57 | No: `d` is the app class | `is_abstract`, name span | (app AST) |
| collect/shapes.rs:38-40 `shape` | No in practice: `cx.shaped` (cloned from std's stage) holds every std type, and it is checked first | `decl.is_some()` | DeclFacts.declared |
| collect/shapes.rs:300/325/405 (`iface_shape`/`adt_shape`/`enum_shape`) | No: reached only through `shape`, which returns for shaped (std) types; `ensure_fields` returns at `shaped && laid_out` | the whole declaration | none needed |
| **collect/shapes.rs:387** (base-class check in `adt_shape`) | **Yes**: an app class `extends` a std class | the base's `ctor_visibility == Private` | DeclFacts.ctor_visibility |
| collect/sigs.rs:417 `adt_methods`, :559 `iface_methods` | No: `resolve_def_sigs(items)`, app items only | methods/ctor | (app AST) |
| collect/iface_extends.rs:22 `flatten_from` | No: `from..` app ifaces | `decl.is_some()` | - |
| collect/iface_extends.rs:116-117 `inherit_field` | No: `d` is the app iface being flattened (parents before `from` are only read: `done`) | own `decl.fields.len()`, `decl.is_none()` | DeclFacts.own_field_count / declared |
| collect/field_only.rs:26 `declare` | No: `from..` | presence | - |
| **collect/field_only.rs:89 `is_field_only`** | **Yes**: it recurses into parents (`parent_iface(cx, module, t)` for each `extends` entry), so an app interface extending a std interface reads the std parent's `decl.methods.is_empty()`, `decl.fields`, and **resolves the std parent's `extends` type expressions in the std module** | `methods.is_empty()`, `fields.is_empty()`, `extends` type ASTs | std's stage already decided: `cx.field_only.contains_key(&p)` for a frozen `p` (the memo can be seeded from it). No AST is needed |
| collect/field_only.rs:136 `fill` | No: pairs with `i.0 >= from` | own field count | DeclFacts.own_field_count |
| collect/extended_layouts.rs:326 `plain_extension` | Presence only: for a std iface, `i.decl?` then `is_std` short-circuits before `decl.extends`; for the base it reads only `cx.scopes[..].is_std` | `decl.is_some()` | DeclFacts.declared |
| collect/extend_shadow.rs:39 | No: `from.defs..` | presence | - |
| collect/c_struct.rs:22 `is_c_struct` (helper) | **Yes**: called on any type (`check_finite`, `copy_types`, field checks) | `c_struct` | DeclFacts.c_struct (0 in std) |
| collect/c_struct.rs:34 `check_all` | No: app items | whole C struct decl | (app AST) |
| collect/c_struct.rs:165 `field_ty` | Yes: an app C struct's field of a std type | `c_type` | DeclFacts.c_type |
| collect/c_sig.rs:412 `c_pointee` | Yes: an app `declare c` signature with `Ptr<StdType>` | `c_type \|\| c_struct` | DeclFacts flags |
| decorators/mod.rs:136, :146, :204 | No: `check(cx, from.defs)` iterates app defs, and `AbstractProperties::look` takes the owner of an app method | type/field decorators, abstract/property methods | (app AST) |

## Type resolution and defaults (run whenever an app annotation or `new` names a std type)

| Site | Std type in the app's stage? | What it reads | Replacement |
|---|---|---|---|
| **resolve.rs:579 / :585 `def_type`** | **Yes**: every app annotation naming a std class or interface | `(module, &decl.generics)`, passed to `with_defaults`, which reads only `gs.len()` and `g.default.is_none()` per parameter; the resolved defaults are memoized in `type_defaults.resolved`, filled for every std declaration with defaults by `check_from` in std's stage | owned `Vec<bool>` "has default" per type parameter, or let `with_defaults` look up `type_defaults.resolved[key]` first and read the generics count from `a.generics` |
| resolve.rs:737 `adt_with_defaults` | Yes (`new StdClass<..>`) | same | same |
| type_defaults.rs:100 `adt_param_defaults` | Yes (`construct.rs:132`, `static_call.rs:213`, `downcast.rs:251` on std classes) | same; memoized values | same |
| type_defaults.rs:46/50 `check_from` | No: `skip(from.defs)` | generics | - |
| utility_types.rs:331 `iface_fields_now` | Yes: `Partial<StdIface>` and similar while the app is being collected | own `decl.fields.len()` | DeclFacts.own_field_count |

## Body checking (app bodies using std types)

| Site | Std type in the app's stage? | What it reads | Replacement |
|---|---|---|---|
| creation_notes.rs:14 `ctor_visibility` | Yes | the ctor owner's `ctor_visibility` | DeclFacts.ctor_visibility |
| body/expr/access.rs:118 `check_ctor_access` | Yes (`new StdClass()`; 5 private/protected ctors in std) | `ctor_visibility` | DeclFacts.ctor_visibility |
| body/expr/construct.rs:61, :73 | Yes (`new StdType()`) | `c_type`, `is_abstract` | flags |
| body/expr/ctor_values.rs:143 | Yes (a std class passed as a construct-signature value) | `is_abstract` | flag |
| **body/expr/ctor_objects.rs:265 `class_object`** | **Yes**: printing or using a std class's object (`console.log(Map)`) | static fields (not `#`, not `<static block`) and static accessors, by name | owned bool `has_shown_statics` per class |
| **body/expr/regex_args.rs:210 `field_regex`** | **Yes**: `stdObj.field` used as a regex argument walks the std class and its bases | the field decl (`is_static`, name, `readonly`, **default expression**), and **the constructor body** (`this_fields_assigned`) | owned per class: `regex_fields: Vec<(name, (groups, shape))>` for readonly fields with a literal regex default that the ctor doesn't assign, computed in std's stage (`literal_shape`). Rare; could also stay per-declaration AST |
| **body/expr/system_args.rs:204** | Yes: a system/host call passing a field of a std class (walks bases) | the declaring field's **written type AST** (`is_readonly_written(&f.ty)`) | owned per-field bool `written_readonly` on `FieldInfo` |
| enum_keys.rs:18 `enum_key` | Yes: `Enum.Member` naming a std enum (only 2 in std) | variants' discriminant **expressions** (`const_str`/`const_int`) | `VariantInfo.discriminant` / `str_value` (already owned) |
| iface_writes.rs:219, :267 | Yes: writes through std interfaces | `decl.is_none()` / presence | DeclFacts.declared |
| body/driver.rs:85-90 `field_defaults` | No in practice: `field_defaults_checked` (cloned) holds every std ADT, since std's `check_bodies_from(0)` visits all std defs before the app; callers `object.rs:630`, `construct.rs:285`, `throws/mod.rs:119` therefore return early | field default ASTs | none (keep the memo set in the frozen unit) |
| body/defaults.rs:311/323 `iface_defaults` / `own_iface_defaults` | No: `from..` | method default ASTs | - |
| **body/defaults.rs:367/387 `inherited_iface_defaults`** | **Yes**: an app interface extending a std interface reads the parent's `decl.methods` to see whether the parent declares method `m` itself | own method keys of the parent | DeclFacts.own_method_keys (or own-methods count, since `IfaceInfo.methods` lists own methods first) |
| body/expr/proxy.rs:302 | Yes | `decl.is_none()` | DeclFacts.declared |

## Passes after the bodies and the program build (these walk every type or def)

| Site | Std type in the app's stage? | What it reads | Replacement |
|---|---|---|---|
| copy_types.rs:26 `declared_struct`, :73/:83 `is_copy_depth` | Yes: on any type | `decl.is_some()`, `c_struct` | flags |
| struct_tuples/mod.rs:58/64 `check_instantiations` | Yes: walks the whole `cx.ty.table` from 0 | `decl.is_some() && !c_struct`, **`decl.name.name`** (message) | flags + owned name (`a.name` likely the same; check the qualified vs. short form) |
| struct_tuples/mod.rs:151 `stored_types` | Yes | flags | flags |
| finalize.rs:269-270 `adt_def` | Yes: `build_defs` builds every def, std's included | `decl.is_some()`, `c_struct` | flags |
| ownership/exclusive/mod.rs:658 `by_identity` | Yes | `decl.is_none()` | DeclFacts.declared |
| ownership/local_async/targets.rs:467 `object_type` | Yes | `decl.is_none()` | DeclFacts.declared |

## Summary of kinds

1. **Presence and flags** (about 20 sites: declared, c_struct, c_type, is_abstract, ctor_visibility):
   every std type can hit these, through `is_copy`, `finalize`, `struct_tuples`, `new`, and others.
   Replace with an owned `DeclFacts` filled when the type is collected.
2. **Counts and keys of own members** (utility_types:331, iface_extends:117, field_only:136,
   defaults.rs:387, classes.rs:155 `override` keys, abstracts.rs:68 abstract keys): owned counts
   and key sets.
3. **Type-parameter defaults presence** (resolve.rs:579/585/737, type_defaults.rs:100): the values
   are memoized already; only `has_default` per parameter is needed.
4. **Real AST content** (riskiest):
   - field_only.rs:89: resolves a std parent's `extends` types. Use std's `field_only` map instead.
   - regex_args.rs:210: a std field's default literal and the ctor body. Compute an owned regex
     shape per field.
   - system_args.rs:204: a std field's written type (`readonly`). Owned per-field bool.
   - ctor_objects.rs:265: std class statics. Owned bool.
   - enum_keys.rs:18: discriminant expressions. `VariantInfo` already has them.
   - abstracts.rs:68: `decl == None` breaks the base walk. `DeclFacts.declared` must stay true
     for std types.

---

# M2 audit, part: globals, deferred sigs, module ASTs (app's stage, `VELT_UNITS=2`)

Tree: `/Users/andii/dev/velt/branches/d1-open-pr`, HEAD acddf0f0c. Another session committed while this
audit ran: `units.rs` is now `units/mod.rs` (+ `units/codec*`). Line numbers are against that HEAD.
Paths are relative to `crates/velt_sema/src`.

Ground facts used below:
- Every std global is `BodyState::Done` after std's stage (`check_bodies_from` runs `ensure_global` on all
  defs from 0; units loads all of std, so `lazy_std::deferred` is false). `ensure_global`
  (body/driver.rs:155) returns on `Done`, so the initializer check at body/driver.rs:206–209 never runs
  for std in the app's stage.
- Name resolution in a std module (`lookup_item[_at]`, `lookup_path_at`, ctx_lookup.rs:26,
  namespaces.rs:170/182) reads only `cx.scopes[m]`, `cx.nested` and `cx.modules[m].file`. It reads no AST.
  So "resolve in std module m" needs std's `ModuleScope`s and the modules' metadata, not their ASTs.
- `SourceModule` metadata (`path`, `is_std`, `is_dependency`, `file`, `imports`, `jsx_runtime`) is read
  for std modules all over the place (linked.rs, module_state.rs:133, exports.rs:84 `std_module`,
  generated/*.rs `position(|m| m.path == …)`, known.rs:96/161, decorators/mod.rs:150, symbols.rs:322,
  `add_app` → `import_targets`). A frozen unit must carry it, but none of it is AST.

## (a) `GlobalSrc.ann / .init / .c_global` (the `'m` borrows)

`GlobalSrc.span/owner/is_private/c_const` are owned already, and so are `GlobalInfo.init` (hir::Expr) and
`GlobalInfo.c_global` (hir::CGlobal). Sites that read only those are not listed.

| Site | In app's stage for a std global? | What it reads | Owned replacement (computed in std's stage) |
|---|---|---|---|
| type_queries.rs:140 `query_def` | **yes**: `typeof STD_CONST` in an app annotation | `ann` (re-resolved in the std module) and `init` (literal / `Symbol(...)` shape) | `query_ty: Option<TyId>` per std global: the type `typeof` gives (ann resolved, literal type, `symbol`) |
| unique_symbols.rs:29 `unique_symbol_type` | **yes**, via `query_def` (memoizes positives only) | ann is `unique symbol` / init is `Symbol(...)` | precompute `unique_symbols` for every std global (or a `is_unique: bool`) |
| symbols.rs:152/191 `known_symbol_depth`, `description` | **yes**: app computed key `[STD_KEY]` (`resolve_key` symbols.rs:296ff), and app `const A = STD_KEY` chains (depth recursion) | init: `Ident` alias chain, `Symbol()`/`Symbol("d")`/`Symbol.for("k")`, description literal or const | `known_symbol: Option<KnownSymbol>` per std global (`name_symbol_keys` already runs over std in std's stage, symbols.rs:274) |
| symbols.rs:213 `symbol_init` | no: runs inside `ensure_global` of the global being checked (app only) | init span | none needed |
| symbols.rs:346 (`member_key` const string) | **yes**: `[STD_CONST]` key naming a std string const | init literal text | `const_text: Option<String>` per std non-mutable global |
| names.rs:429–433 (untyped int const read) | **yes**: app reads `const N = 4` from std; **re-checks std's init AST in the app function's context** (`self.expr(init, …)`) | init expression (an untyped int literal) | `int_lit: Option<ast::Lit>` (the literal alone); then build the hir literal in place |
| names.rs:442, 489; unsafe_code.rs:108; module_state.rs:371; c_sig.rs:243–244 | yes (names/unsafe/module_state); c_sig only for app globals (`made_init` within `ensure_global`) | `c_global.is_some()`, and `writable` (unsafe_code.rs:108) | `c_global: Option<bool /*writable*/>` (drop the `&CLibrary`; `GlobalInfo.c_global` holds the checked one) |
| widening_consts.rs:206 `global_widening_fn` | **yes** (`f()` of a std const fn value) | init span (key into `widening_values`) | `init_span: Option<Span>` |
| widening_consts.rs:262–265 `widening_global` | **yes** (app names a std const) | ann/owner/init shape: ident chain or "conditional of plain literals" | `widening: bool` per std global (follow the chain in std's stage) |
| switch/select.rs:101–105 `const_lit` | **yes**: `case STD_CONST:` | ann/owner, init literal | `case_lit: Option<ast::SignedLit>` |
| literal_patterns.rs:396 `as_const_reads` | **yes**: app destructures a std `const X = [...] as const` | the init's `as const` tuple **elements as AST** (checked again in the app body) | per-declaration AST: clone of that one init (only for `as const` tuple consts), or the checked hir elements |
| generic_conversions.rs:194 `named_fn_def` | **yes**: app uses a std `const g = genericFn` | init is an `Ident`, resolved in the std module | `alias_of: Option<DefId>` |
| system_args.rs:170 `is_readonly_source` | **yes**: app passes a std global to a system function | ann is written `readonly …` | `ann_readonly: bool` |
| throws/fn_flows.rs:360 `elem_fn_site` | **yes** (indexed read of a std constant array of functions) | ann span | `ann_span: Option<Span>` |
| decorators/consts.rs:234 `global_value`, decorators/suffixes.rs:176 `named` | **yes**: `@dec(STD_CONST)` in app code | init expr evaluated as a constant (std module scope) / searched for a wrong suffix | `const_value: Option<ConstTree>` (+ suffix facts) per std const, lazily or for the consts a decorator can name |
| body/driver.rs:206–209 | no (std globals `Done`) | ann, init, span | none |
| lazy_std/mod.rs:325 | lazy mode only (owner only, owned) | none | none |

Counts: about 1,976 module-level `const` lines in std (`^(export )?const`, 270 files), 102 of them
`= Symbol…`, 83 untyped int constants (`const X = <int>`). Only the constants an app names reach
these sites, so few per program. The owned summary is about 10 small fields per global. All of these
together are much smaller than keeping `init` ASTs. The one exception is `as const` tuples
(literal_patterns.rs), which want the elements.

## (b) `SigQueries::deferred_sigs`

No std entries survive. `collect_std` ends with `sigs::fill_deferred` (collect/mod.rs:107), which loops
until the map is empty (collect/sigs.rs:71–75; `fill_pending` removes each entry). In the app's stage,
only `resolve_def_sigs` over the app's items inserts entries (collect/sigs.rs:58), and
`collect_added_in` drains them again (collect/mod.rs:187). `body/returns.rs:62` only checks whether the
map is empty. Replacement: none. The frozen unit can assert that the map is empty, and the field may
stay `'m` (app only) or become owned per declaration.

## (c) Module AST reads reachable from the app's `Ctx`

| Site | Runs for std modules in the app's stage? | What it reads | Owned replacement |
|---|---|---|---|
| units/mod.rs:169 `add_app` → collect/exports.rs:401 `import_targets(&cx.modules)` | **yes, every std module**, but metadata only (`SourceModule.imports`, `path`). Not AST. Keys borrow `&'m str` | spec → path | keep std's part of `import_targets` from std's stage (with owned `String` keys) and append the app's, or carry `imports` in the frozen unit |
| script_tdz.rs:22–37 `check` (units/mod.rs:54 calls it first in the app's stage) | **yes, every std module**: loops over `cx.modules`. std's stage never runs it | `module_scripts::script_fns(m)` (top items), `export default` var, `early_uses(items, stmts)` | run it for std's modules in std's stage and only for `app_base..` in the app's. Its `promoted_consts` for std are useless in the app anyway (std's constants are collected already: a semantic gap). At most a per-module flag "has scripts/default export" |
| module_scripts.rs:56–65 `declare` (lib.rs:685, in `analyze_after_phase`) | **yes, every module but the root**: runs in std's stage *and again* in the app's, so a std module with top-level statements would get a **second** `<top-level statements>` global (a possible bug; std may have none) | `script_fns(m)`: the module's items | limit to the app's modules (`app_base..`). std's slots are declared in std's stage. Owned fallback: per module, the names of its script functions |
| module_state.rs:224 `evaluates` (from `module_order`, :133, from `order`, lib.rs:730) | **yes**: every std module the root reaches over imports | the module's `Import` items: is the import of `spec` a value import (not `import type`, side effect, …) | per module: `Vec<(spec, evaluates: bool)>`, e.g. a flag next to `SourceModule.imports` |
| collect/exports.rs:507 `imports_of` (the choke point; 20 callers in exports.rs, export_walk.rs, imports.rs, default_snapshots.rs) | **yes**, whenever the app imports from a std module whose export table std's stage did not build (a public `index.velt` that no std module imports), follows a re-export chain without a table (`export_at`/`local_at`, cycles past `MAX_DEPTH`), or after `Memo::changed()` (namespace_bound, exports.rs:64, when an app export list names a bound namespace) clears **every** table, std's included | each `Import` item: `from`, names (name/alias/type_only), `exported`, `all`, `namespace`, `type_only` | (1) build `Memo.tables` for **every** std module in std's stage. (2) Make `changed()` drop only the app's modules' tables. (3) Give each std module an owned import record list (`OwnedImport { from, names, exported, all, namespace, type_only }`), which `imports_of` reads instead of the AST. 62 `index.velt`, about 273 re-export lines, about 2,520 import lines in std |
| collect/exports.rs:254–282 `any_exported_namespace` | **yes, every std module**, once per app stage: the memo is keyed on `cx.modules.len()`, which grew | items: `export * as`, `import * as` plus an export list | keep std's bool from std's stage and OR it with the app's modules' (std has 0 `export * as` lines) |
| collect/imports.rs:24 `resolve_imports(cx, m)` | app modules only (`declare_added(ms)`). It reaches std's ASTs only through `export_of`/`all_exports`/`namespace_exports`/`imports_of(t)` (row above); `exported_item` reads `imports_of(cx, t)` of a std `t` directly on the `default` error path | as above | as above |
| collect/declare.rs:69 `declare_added`, collect/nested.rs:49, collect/sigs.rs:22–33 (`from.modules..` with the `unit` filter) | no: app modules only | none | none |
| collect/default_snapshots.rs:17 `declare` | no: it skips std, and it is called only from `declare_all`/`declare_std`, **not** from `declare_added`. The app's `export default x` snapshots are missing in units mode (aside: a likely bug) | none | none |
| collect/sigs.rs:94 `extern_sig(cx, d, module)` | **yes**: from type_queries.rs:262 for `typeof stdDeclaredFn`. It scans the std module's items for the `ExternFn` at the def span | the extern fn signature's params (rest/optional/default) | the owned param flags the FnSource audit wants anyway |
| js_api.rs:46–104 `js_api_modules` (OnceCell; `is_js_api` is false when the caller is std, so std's stage never fills it) | **yes**, on the first app call of a std function: all `std/prelude/global` modules' `Import` items, plus `defining_module` re-export chains through std modules' items | import names and re-export targets | compute the map in std's stage (it depends on std only): `FxHashMap<usize, Option<FxHashSet<String>>>` |
| generated/mod.rs:304 `has_generated_fns` (phase.rs:73, mod.rs:351) | **yes, every module's items**, at each call in the app's stage | is there a `declare generated(…)` fn | std's answer as a bool (true: std has 13), OR the app's modules; or `!generated_fns().is_empty()` |
| generated/mod.rs:293 `generator_of` via mod.rs:602 `gen_function` | **yes**: the app's stage generates for std's demands (`JSON.stringify` and the like). It clones the std fn's `GeneratorSpec` | `sig.generator` (`spec: String`, `span`) | owned `Option<GeneratorSpec>` on FnInfo (13 std decls) |
| generated/phase.rs:184 | no: the driver's generated batch modules | none | none |
| decorators/scope.rs:37 `Scopes::of(module.ast.items)` | no: called for the module a decorator is written in; `decorators::check(cx, from.defs)` covers app defs only | none | none |
| main_fn.rs:16, lib.rs:491 (`moves_more`), script_locals.rs:37/196 | no: root only (an app module) | none | none |
| body/expr/fn_shape.rs:58/219 `ast.fn_name(span)` | no: `self.module` is the body being checked (app bodies; std's are `Done`) | none | none |
| body/expr/proxy.rs:285, this_fn_call.rs:188, json/handles.rs:41 | no: `filter(!is_std)` | none | none |
| replaced_holders/fix.rs:48 `written` | only when reporting an error whose assignment is in std code (the candidates come from all fns, mod.rs:424). A clean std never gets here unless the app makes a std assignment wrong | that assignment's op and rhs | skip the fix note for spans outside the app (or keep the AST of that one declaration) |
| kept.rs, kept_reload.rs | no: a kept check requires `order.is_none()` (lib.rs, `kept::wanted(…) && order.is_none()`) | none | none |
| program.rs `build` → finalize.rs, layout_merge.rs, readonly.rs, module_state::build, type_level::lowered_ops | no module AST. finalize.rs:61 reads `modules[m].path`; finalize.rs:269–270 read `AdtInfo.decl` flags (the parent's `.decl` audit) | none | none |
| `generated::Gen<'m>` (generated/mod.rs:233) | `generate: Option<&'m Generator>` is the driver's generator, installed on the app's copy (`generating::install`). It is no std AST | none | not saved. Re-installed per check, as today |
| `std_load::Loads<'m>` (std_load.rs:101) | std's stage is fresh and sets `cx.loads = loads`, so the snapshot holds the driver's callback. With all of std loaded, nothing is served | the driver's callback and batches | not saved. Reinstall (or `None`) on the app's copy. Lazy mode only |
| global_this::declare / check_sites (global_this/mod.rs:296/312) | no AST: `global_props.homes` and `sites` own their `TypeExpr`s (the rewrite skips std) | none | none |
| linked.rs (all) | no AST: modules' `path`/`imports`/`file`, and std's `module_edges` kept in `unit_sums` | none | none |

## Pre-`Ctx` source rewrites (lib.rs:278–339 `check_timed`, and `script_locals::check` around it)

M2 needs these not to read std's ASTs, or else a check still decodes all of std before sema starts.
"Scans std" means the pass looks at every std module's AST at least once per check, every round.

| Pass | Scans std? | Rewrites (clones) std? | Feeds std's unit key (`rewrite_facts.rs:72` `std_key`) |
|---|---|---|---|
| script_locals::check (lib.rs:254) | root only | no | none |
| module_chunks::split (module_chunks.rs:36) | **yes**: every non-root module (`split_module`) | when any module needs it, every module is cloned | `promoted` spans in std's files |
| asserted_targets::rewrite | `par::map` over all modules, but `has_asserted_target` returns false for std at once (:94) | no | `rmw` spans |
| call_sigs::check | no (`!is_std`, :359) | none | none |
| call_sigs::rewrite (:89) | **yes**: `any(callable)` over every module's items, then a `par::map` over all | clones all when any is callable | diags |
| module_destructure::rewrite (:27) | **yes** (`any(has)` over all) | clones all when any has one | none |
| namespaces::global_namespaces (:38), flatten (:63), global_members (:253) | **yes**: every module's items | clones all when any has a namespace | `ns_private`, `global_ns` (std's part, `Order::std_facts`) |
| interface_merge::rewrite (:28) | **yes** (`any(needs_rewrite)`) | clones all; it may *augment std modules* (`augment_global`, `declare module` merges into another module, :92–122), so app code changes std's ASTs | diags |
| generic_arrows::lift (mod.rs:44, typed.rs:70) | **yes**: every module, and `GenericFnTypes` indexes every module's aliases and imports | rewrites lifted modules | `lifted` spans in std's files |
| overloads::rewrite (:53, :155, :244, :263) | **yes** | clones all when any has overloads | `methods` |
| recursive_aliases::rewrite (:32) | **yes**: every module's items | rebuilds all | `alias_blocked` spans |
| global_this::rewrite (mod.rs:108) | **yes**: `typed(m)` (declared global types) and `string_consts` over every module; it rewrites only non-std modules, but it clones every module and strips std's `declare global` aliases (:139–145) | std's `declare global` types are removed from std's items | `global_props` (remapped) |
| defined_accessors::rewrite | no (`!is_std`) | clones all when any non-std module needs it | `record_accessors` flag |
| defined_props::rewrite | no (`!is_std`, :69/:125) | clones all when needed | `brands` (remapped) |
| nested_closures::rewrite (:73) | **yes**: every module, twice (`par::map`) | rebuilds all when any has one | `kept`, `groups` in std's files |
| source_scans::program | no (`!is_std`) | none | `null_proto`, `ctor_reads`, `record_accessors`, `gen_throw` come from the **app's** scan, yet they are part of std's key (rewrite_facts.rs:103), so std's unit depends on the program |

So 9 rewrite passes walk every std module per check: module_chunks, call_sigs::rewrite,
module_destructure, namespaces (3 walks), interface_merge, generic_arrows (lift and typed),
overloads, recursive_aliases, global_this (typed and string_consts), and nested_closures. For M2 they
have to run over std once when std's unit is built, with their std results kept in the unit, and then
over the app's modules alone. interface_merge (app `declare module`/`declare global` augmenting std
modules) and global_this (std's `declare global` types) are the two where the app's rewrite changes
std's syntax. They are the riskiest.
