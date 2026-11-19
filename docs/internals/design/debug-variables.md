# Variables in the debugger

Status: implemented. Issue: [#958](https://github.com/velt-lang/velt/issues/958).

## Summary

Debug builds made for a debugger describe each source variable in DWARF: its name, where it lives,
its source type and the part of the function where it is in scope. LLDB formatters shipped with
the toolchain show Velt values as a TypeScript programmer reads them: strings as text, arrays as
their elements, options as `null` or the value, and tagged unions as their active variant.

## Motivation

Without variable descriptions a debugger stopped in a Velt function shows no locals, and raw
memory would not help: a `string` is three words with an inline form, an array is
`{ data, len, cap }`, and a class value is a pointer to an object whose first word may be a
vtable.

## Design

Variables are described only in debug (Cranelift) builds made for debugging: `velt build`,
`velt build --json` (what the VS Code extension's F5 runs), `velt dev --exe` and
`velt test --no-run`. `velt run` and `velt test` do not describe them, because keeping each
described variable in a stack slot costs run time. Release builds are unchanged.

A variable is in scope from its declaration to the end of the block that declares it; parameters
and the variables of a function's own block are in scope until the function ends. A debugger hides
variables not yet declared and those of a finished loop.

A value set from the debugger (`expr x = 100`) is written to the variable's stack slot, which the
program does not read back: it keeps using its own copy of the value.

## Compilation

### VIR describes variables

Lowering with `LowerOptions::debug_info` (`crates/velt_vir/src/lower/debug_vars.rs`) attaches a
`LocalDebug` to each local that holds a source variable: its declaration, the end of its block
(`LocalDebug::end`), its debug type, whether the local holds a pointer to the value (a
by-reference parameter, a captured variable, a variable in a shared cell) and whether it is a
parameter. It also builds `Program::debug_types`: one `DebugTy` per source type, with its source
spelling (`Point | null`) and its layout in terms of the aggregates lowering made (field names over
VIR field indexes, the variants of tagged types). The types are in
`crates/velt_vir/src/vir/debug.rs`; invariant 10 in `crates/velt_vir/src/vir.rs` states the
contract. Without the option, `debug_types` is empty and every `LocalDecl::debug` is `None`.

Optimizations keep the descriptions valid: dead-code elimination keeps described locals, and
`numrep` (`crates/velt_opt/src/numrep/rewrite.rs`) moves a description to the local a `number`
is narrowed into, so backends take a scalar's encoding from the local's VIR type. A function
shared across instances by shape (see [shape sharing](shape-sharing.md)) gets a `$types`
parameter naming the concrete types of the frame; values of placeholder type are described by
their layout.

### Cranelift emits DWARF

A described scalar stays in a register for its reads, and each assignment also stores it to a
shadow stack slot; aggregates live in stack slots anyway, and `StackSlotKey` finds the slots after
compilation. A described local keeps a keyed slot of its own; other memory locals share slots when
their live ranges are disjoint (`crates/velt_codegen_cl/src/function/slots/`).

Each variable becomes a `DW_TAG_variable` or `DW_TAG_formal_parameter` with a frame-base location
(plus `DW_OP_deref` when the local holds a pointer) and a DWARF type built from its debug type:
base types, structures with named members, pointers to class objects and enumerations
(`crates/velt_codegen_cl/src/debug_info/`).

Scopes come from source ranges. Those ranges nest like the blocks, so each variable goes in a
`DW_TAG_lexical_block` inside the block of the innermost variable whose range holds its own,
covering the code located in its range. Code without a source location (a few bytes between
located instructions) belongs to the code before it; leaving it out would cut every scope around
it, and a function of n declarations in a row would need n² ranges. For code laid out far from
source order, a function's scopes get at most a number of ranges proportional to its code
(`scope_range_budget`); a variable that does not fit is placed in the scope around it, visible
from before its declaration.

### LLDB formatters

`editors/lldb/velt_lldb.py`, installed as `share/velt/lldb/velt_lldb.py` and loaded by the VS Code
extension, matches debug type names. A value that cannot be valid (a variable not initialized yet)
shows as `<invalid string>` or `<invalid array>` rather than being read as a huge array. The
formatters need no recognizer functions, so they work with LLDB 18.

## Runtime cost

None in release builds or in `velt run`. A build that describes variables stores every assignment
of a described scalar to its stack slot as well as keeping it in a register.

## Alternatives considered

**Cranelift's value locations (`set_val_label`).** At Cranelift's optimization level they lose
most values between uses, so a debugger shows loop counters and short-lived locals as
unavailable. A shadow stack slot is always readable.

**Field names in a side table indexed by aggregate.** It needs no new types, but tagged types,
options and the source spelling of a type still need a description, and the table would describe
layouts rather than source types.

**Scopes as statement ranges.** After CFG lowering a scope is not a contiguous range of statements.

**A scope id in every source location.** Exact, but it touches every location and every pass that
makes one; a source range per variable needs neither, since the code of a block is the code
located inside it.

## Limitations

- Locals of async functions and generators are not described: their values live in the task's
  state machine, not in the frame.
- Inlined code's variables are not described (`crates/velt_opt/src/inline/splice.rs`): without
  inlined scopes a debugger would show them in the caller's scope.
- Only the Cranelift backend emits variables. Release builds with `-g` keep line information
  only, so their optimizations are unaffected.
