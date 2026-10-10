# Design: variables in the debugger

Status: in progress (issue #958, phase 2). Phase 1 made F5 build and start a debugger with
breakpoints and stepping; this phase shows local variables with readable values in debug
(Cranelift) builds.

## Problem

Debug builds emit DWARF line tables and functions only. A debugger stopped in a Velt function
shows no variables, and if it did, Velt values would be raw words: a `string` is three words
with an inline form, an array is `{ data, len, cap }`, a class is a pointer to an object whose
first word may be a vtable.

## Proposal

Three steps, each its own pull request:

1. **VIR describes variables** (this note's contract change, vir/debug.rs, vir.rs invariant 10).
   Lowering with `LowerOptions::debug_info` attaches a `LocalDebug` (declaration, debug type,
   held by reference, param) to each local that holds a source variable, and builds
   `Program::debug_types`: one entry per source type, with its source spelling (`Point | null`)
   and its layout in terms of the aggregates lowering already made (field names over VIR field
   indexes, variant views of tagged types). Optimizations may store a `number` local as an
   integer, so backends take a scalar local's encoding from its VIR type. Only debug builds made
   for debugging ask for it (`velt build`, `velt dev --exe`; the next step keeps described
   variables in memory, which slows hot loops by about a quarter, so `velt run` and `velt test`
   don't), and release code is unchanged; dead-code elimination keeps described locals, and
   `numrep` moves a description to the narrowed local.
2. **Cranelift emits them.** Described locals live in stack slots (`StackSlotKey` finds them
   after compilation); each becomes a `DW_TAG_variable` / `DW_TAG_formal_parameter` with a
   frame-base location (plus `DW_OP_deref` when held by reference) and a DWARF type built from
   its debug type: base types, structures with named members, pointers to class objects,
   enumerations.
3. **LLDB formatters** (`share/velt/lldb/velt_lldb.py`, loaded by the VS Code extension) show
   strings as text, arrays as their elements, options as `null` or the value and tagged types
   as their active variant, matching debug type names. Lexical scopes come with this step, so
   shadowed and not-yet-declared variables are hidden.

Locals of async functions, generators and inlined code are not described yet (phase 4: their
values live in state machines or the caller's frame).

## Alternatives

- Field names in a side table indexed by `AggId`: needs no new types, but tagged types, options
  and the source spelling of a type still need a description, and the side table would describe
  layouts rather than source types.
- Scopes as statement ranges: after CFG lowering a scope is not a contiguous range, so scopes
  will be recorded per statement location instead (step 3).
