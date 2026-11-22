# Printing typed values

Status: partly implemented. Issue: [#1967](https://github.com/velt-lang/velt/issues/1967).
Related: [lazy std](lazy-std.md), [string building](string-building.md).

## Summary

`console.log`, `util.inspect`, `String(x)` of an object, template literals and `assertEq`'s
message print values the way Node does. What every type prints the same way (the depth limit,
arrays, `... n more items`, `<ref *N>` and `[Circular *N]`) is std code, written and checked
once. What differs per type (which fields show and in what order, each key's text, the type's
name, which member a union holds) is written by lowering from the type, as it computes the
type's layout. Printing involves no source generator, no generated module and no demand per type,
so a build on an empty cache costs what a warm one does.

## Design

`inspectNested<T>(buf, v, depth)` is an intrinsic (`Intrinsic::InspectNested`, declared in
`crates/velt_sema/src/intrinsic_decls.rs`) that lowering binds per instance: a leaf value is
appended in place, and anything else calls the type's format function (`Glue::Format`, one per
type, with the format slot's signature). Lowering's format glue (`crates/velt_vir/src/lower/glue/
format*.rs`) writes the per-type parts around calls to std hooks, all appending to the line's own
string builder, so a printed value has no builder of its own. Static key texts are appended as
static bytes (`push_text`).

### What std holds

The hooks are declared in `crates/velt_common/src/std_hooks.rs`.

`std/lang/hooks/inspect.velt`:

| Hook | Writes |
|---|---|
| `openPlain(buf, cut, depth)` | the depth limit of an object that cannot hold itself (`[Name]` past it) |
| `inspectArray<E>(buf, a, depth)` | `[ a, b ]`, the first 100 elements, `... n more items`, `[Array]` |
| `inspectMore(buf, remaining)` | `, ... n more items` after the first 100 entries of an array, `Map` or `Set` |

`std/lang/hooks/inspect_cycles.velt` holds what printing an object that can hold itself needs:

| Hook | Does |
|---|---|
| `openObject(buf, cut, id, depth)` | the depth limit and the cycle check, before the object's fields |
| `inspectEnter`, `inspectLeave` | mark an object as being printed; insert its `<ref *N> ` prefix |
| `inspectCircular` | past the depth limit, `[Circular *N]` when the object is being printed |
| `inspectBegin`, `inspectEnd` | start and end a top-level value: the numbering starts over |
| `inspectSave`, `inspectRestore` | give a custom inspect a state of its own, then restore the outer one |

The state is per thread: a value is printed on one thread with no await in between. Node's depth
limit enters at most three objects at once, kept in thread-locals; the `<ref *N>` numbers live in
a block of words allocated at the first cycle and freed at the end of the top-level value, so the
leak counters stay even. A `<ref *N>` prefix is inserted by taking the builder's text out
(`velt_rt_strbuf_finish`) and appending it again around the prefix, which is as rare as a cycle.

A custom inspect method is user code that can print, and panic, during a print. Lowering brackets
its call with `inspectSave` and `inspectRestore`, so a value it prints is numbered from 1 (Node
starts a fresh context per `console.log`) and the outer value's state comes back after it. A panic
caught by `catchPanic` leaves the thread at that empty state, and `inspectBegin` forgets any
object a panic left marked, so the next value is numbered from 1.

### What stays in lowering

`Map`, `Set` and their subclasses, `Record`, tuples, options and unions (which are dispatch, not
shape), promises, functions, `JsonValue`, the custom inspect hook's result and `any` dispatch
through the format slot are written by lowering's glue (`format_map.rs`, `format_promise.rs`,
`format.rs`). `Map` and `Set` stay there because a hook taking a `Map` would make the `Map` class
live in every program that prints anything, and with it what an `any` may hold.

### Line breaking

A top-level value is written on one line, then broken across lines the way Node's `util.inspect`
breaks it (`breakLength` 80, 2-space indentation, arrays of more than six short entries in
columns, long strings split at their line breaks). Lowering's `format_top_inspect` notes where the
value starts in the line's builder (its byte length, read inline), writes it, and calls std's
`inspectLayout` (`std/lang/hooks/inspect_layout.velt`). That calls `inspectLayoutText`
(`inspect_layout_text.velt`) by role. It returns at once for a value of at most 71 bytes with
fewer than six commas and no line break, which Node never breaks; otherwise it re-reads the text
(`inspect_layout_parse.velt`), measures each entry as laid out and decides each container's
layout bottom up, then writes the text once (`inspect_layout_render.velt`). A changed text
replaces the builder's tail.

Re-reading the printed text keeps the writers simple. Node decides to break only after it has
formatted every entry, so laying out while writing would make every writer (lowering's glue,
`inspectArray`, `JsonValue`'s, `Map` and `Set`) record each entry's place on every print, short
or not; and `inspectLeave` inserts a `<ref *N> ` prefix after an object is written, which moves
the text after it.

Widths are Node's: the 80-column limit counts UTF-16 units, an array's columns count display
columns (`inspect_layout_width.velt`), with each code point's width from Node 22's own
`getStringWidth` (ICU), generated by `scripts/gen-inspect-width.js` into
`inspect_layout_width_table.velt`. The table is per code point while Node normalizes the text to
NFC first, so the two differ where NFC joins code points into one of another width.

The text a custom inspect returned is kept as one piece, as Node inserts it. Lowering reports each
such piece (`inspectAtom`), and the pieces are a stack with a floor
(`inspect_layout_atoms.velt`): lowering raises the floor around each custom inspect call
(`inspectAtomsSave`, `inspectAtomsRestore`), so a value the custom inspect prints sees only its
own pieces and the outer value keeps its own, and each top-level value starts by dropping the
pieces above the floor (`inspectAtomsBegin`). `inspect_layout_text.velt` is a prebuilt module
(std-object.md "Prebuilt hook modules") with the stack as its private state.

The layout's hooks are checked only once live code prints, at the top level, a value that may hold
containers (velt_sema `live_constructs.rs`, `LAYOUT_HOOKS`); `Format` alone is present for any
class or array in live code. Where they are unchecked, which happens only in programs built without
std, a value stays on one line.

A `JsonValue` printed at the top level is broken by the runtime's own layout, which
`velt_rt_strbuf_push_inspect_json` calls, until `console.log` prints it through std.

## Runtime cost

Each hook is checked only where a program may need it; the list is `INSPECT_SHAPES` in
`crates/velt_common/src/std_hooks/on_demand.rs`. The hooks are demanded by the `Format` and
`Dynamic` constructs, but `Format` is present for any array, struct or class in live code, printed
or not, so sema narrows two groups further (`crates/velt_sema/src/body/lazy_std/
live_constructs.rs`):

- `inspectArray` is checked only once an array type is live: its `E[]` parameter would otherwise
  make the `Array` class live in a program without arrays.
- The cycle hooks are checked only once a live type can hold itself. A walk over the live types
  looks for a path from a class or struct instance, a `Map` or a `Set` back to itself, through
  fields, elements, entries, union members and each live subclass of a class (a generic class as
  each of its live instantiations, or anything once generic code has an instance at its own type
  parameters). For an `any` or an interface value it follows the types live code puts into one,
  and anything once the program's own generic code puts a type parameter's value into one.

Every hook in both groups has a fallback: where it is unchecked, lowering writes an array with its
own glue, opens an object with `openPlain` (the depth limit only) and tracks no cycle. A program
that prints `{ a: 1 }` or a class of scalars checks `openPlain` and nothing of
`inspect_cycles.velt`, and does not load that module. Both modules are in `ON_DEMAND_MODULES`: a
program that demands none of their hooks neither loads nor checks them.

`bench/inspect/` holds the printing benchmarks: `rows.sh` counts instructions (Linux), `wall.sh`
compares two compilers by wall clock where no instruction counter exists.

## Alternatives considered

**A source generator writing `inspectNested<T>` per type.** It printed Node's output at the target
run time, but its compile time was far over budget: a check of a program with one printed object
type cost +655% on an empty cache and about 80 printed types +18% warm. None of that was the
printing code. A generator host on an empty cache cost about 2.6G instructions, seven times a warm
check; each generated body cost 0.3-0.6M instructions to check whatever its content; and every
program loaded the writers and walked every expression. Lowering already computes each type's
layout, so writing the field sequence there adds nothing to what an object costs.

**Std writers per field count (`objectN`).** One writer per number of fields would let types with
the same field types share an instance, but the 16 writers were most of the module and checking
them cost about 24M instructions in every program that prints.

## Not yet implemented

- **The remaining kinds in std:** `Map`, `Set`, `Record`, tuples, options, unions and the rest of
  the list above move to std hooks, each with an end-to-end test where none prints it.
- **No glue fallback:** once every kind is in std, the `fallback` flag goes from the inspect
  hooks, and with it `format_array.rs` and the entry loops of `format_map.rs`. velt_vir's tests
  that print objects without std move to end-to-end tests.
- **`deepEqual`** moves to the same scheme: std code for what is the same for every type, and
  lowering for the per-type field walk.
