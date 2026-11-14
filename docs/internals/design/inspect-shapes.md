# Design: printing typed values with std shapes bound in lowering

Status: step 1 implemented. Owners: `velt_vir` (`lower/glue/format*.rs`), `velt_sema` (the
intrinsic, lazy std's reach), std (`velt/lang/hooks/inspect`). Issue: #1967. Related:
[lazy std](lazy-std.md), [string building](string-building.md).

## Problem

`console.log`, `util.inspect`, `String(x)` of an object, templates and `assertEq`'s message are
written by format glue that lowering generates in Rust (`glue/format*.rs`). #1967 moves them to
std. The first attempt (draft #1990, a source generator writing `inspectNested<T>` per type) had
Node's output and its run time was within the target, but its compile time was not:

| Program | check warm | check cold | build cold |
|---|---|---|---|
| hello | +2.15% | +2.87% | +2.91% |
| one printed object type | +3.85% | +655% | +615% |
| about 80 printed types | +18.2% | +626% | +322% |
| 30 tests with `assertEq` on objects | +6.73% | +24.2% | |

None of that was the printing code. It had three causes:

1. **A generator host on an empty cache:** about +2.6G instructions, 7 times a warm check, and CI
   runs with an empty cache.
2. **A checked body per printed type:** 0.3–0.6M instructions per function, whatever its body.
3. **What every program loaded:** 825 lines of writers, and a walk of every expression.

Lazy std (on-demand checking on `main`, on-demand loading in #2161) can only remove the third.

## Decision

**Printing is not a source generator.** What is the same for every type is std, written once and
checked once, in a program that may print. What differs per type (which fields show and in what
order, each key's text, the type's name, which member a union holds) is written by lowering from
the type, as it computes the type's layout. There is no generated module, no demand per type and no
host, so a cold build costs what a warm one does.

`inspectNested<T>(buf, v, depth)` is an intrinsic (`Intrinsic::InspectNested`), which lowering binds
per instance: a leaf is appended in place, and anything else calls the type's format function
(`Glue::Format`, one per type, with the format slot's signature).

### What std holds

`std/lang/hooks/inspect.velt` has four hooks (velt_common `std_hooks.rs`) and one private helper:

| Hook | Writes |
|---|---|
| `openObject(buf, cut, id, depth)` | the depth limit and the cycle check of an object |
| `inspectArray<E>(buf, a, depth)` | `[ a, b ]`, the first 100 elements, `... n more items`, `[Array]` |
| `inspectMap<K, V>(buf, m, depth)` | `Map(1) { 'a' => 1 }`, the first 100 live entries |
| `inspectSet<T>(buf, m, id, depth)` | `Set(1) { 'a' }`, the first 100 live members |

`std/lang/hooks/inspect_cycles.velt` keeps what a cycle needs, which lowering's glue and the hooks
above call: the objects being printed and the `<ref *N>` numbers given so far (`inspectEnter`,
`inspectLeave`, `inspectCircular`, `inspectBegin`, `inspectEnd`), and Node's `... n more items`
(`inspectMore`). This was the runtime's (`velt_rt_strbuf_inspect_enter`, `_leave`, `_circular`,
`_more`, deleted, #2196). The state is two blocks of words per thread, allocated at the first
object entered and given back at the end of the top-level value, so the leak counters stay even;
a value is printed on one thread with no await in between. A `<ref *N>` prefix is inserted by
taking the builder's text out (`velt_rt_strbuf_finish`) and appending it again around the prefix,
as rare as a cycle. The runtime keeps `velt_rt_strbuf_inspect_begin` and `_atom` for its line
breaking (`inspect_layout`), which is still Rust.

The design first kept #1990's `objectN` and `objectIdN` writers, one per field count, so that types
with the same field types would share one instance. They were 560 of the 825 lines, and checking
all 16 cost +23.9M instructions. Lowering writing the field sequence is what `main`'s glue already
did, so it adds nothing to what an object costs, and static key texts need no new runtime function:
`push_text` appends static bytes. So the writers and `velt_rt_strbuf_push_static` are gone.

### What a program pays

The hooks list `constructs(Format, Dynamic)`; the cycle hooks other than `inspectBegin` and
`inspectEnd` have no fallback, like the integer hooks, since every object lowering may enter
demands them (velt_common `hook_demand.rs`), so sema checks them
where live code has one of those. `Format` is present for any array, struct or class in live code,
printed or not, because a class's table has a `format` slot, so being demanded is not by itself
cheap: a program with a live class pays for checking the hooks even if it never prints. What
keeps this small is that the module is small.

### What stays in Rust for now

The hooks have the `fallback` flag: where a hook is unchecked, lowering writes the same text with
its own formatter. That covers a program built without std, and a value the live set misses:
live code is not yet a superset of what lowering builds (#2360), and a demand narrower than the
integer hooks' one bit could miss. A miss prints the same bytes.

Still in lowering: a subclass of `Map` or `Set`, `Record`, tuples, options and unions (which are
dispatch, not shape), promises, functions, `JsonValue`, the inspect hook's result and `any`
dispatch through the format slot.

## Order of work

1. **Done here.** The intrinsic and the four hooks, checked on demand.
2. **Loaded on demand.** The module has only functions, so it passes the (a1) rule of
   `velt_loader::std_index`. Once #2345 lands, adding it to `ON_DEMAND_MODULES` removes the load
   cost from a program that demands none of the hooks.
3. **The remaining kinds** in std, each with a golden where none prints it today.
4. **Delete the glue**, once #2360 makes the live set a superset: the `fallback` flag goes, and with
   it `format_array.rs` and the entry loops of `format_map.rs`. velt_vir's no-std tests that print
   objects move to goldens (decision on #1967).
5. **deepEqual** moves to the same scheme (decision on #1967), which removes the equality host
   that `main` builds on an empty cache (about +2.4G instructions).

## Measured

Run time on Windows (wall clock, release builds, interleaved best of 5): `bench/inspect/run_*.velt`
through `bench/inspect/wall.sh`; the numbers are in the pull request. `bench/inspect/rows.sh`
counts instructions on Linux.
