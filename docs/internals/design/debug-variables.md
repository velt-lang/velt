# Design: variables in the debugger

Status: steps 1 to 3 implemented (issue #958, phase 2), step 4 in review (#980). Phase 1 made
F5 build and start a debugger with breakpoints and stepping; this phase shows local variables
with readable values in debug (Cranelift) builds.

## Problem

Debug builds emit DWARF line tables and functions only. A debugger stopped in a Velt function
shows no variables, and if it did, Velt values would be raw words: a `string` is three words
with an inline form, an array is `{ data, len, cap }`, a class is a pointer to an object whose
first word may be a vtable.

## Proposal

Four steps, each its own pull request:

1. **VIR describes variables** (this note's contract change, vir/debug.rs, vir.rs invariant 10).
   Lowering with `LowerOptions::debug_info` attaches a `LocalDebug` (declaration, debug type,
   held by reference, param) to each local that holds a source variable, and builds
   `Program::debug_types`: one entry per source type, with its source spelling (`Point | null`)
   and its layout in terms of the aggregates lowering already made (field names over VIR field
   indexes, variant views of tagged types). Optimizations may store a `number` local as an
   integer, so backends take a scalar local's encoding from its VIR type. Only debug builds made
   for debugging ask for it (`velt build`, `velt dev --exe`; the next step stores each assignment
   of a described variable to a stack slot, +7–15% instructions on benchmarks, so `velt run` and `velt
   test` don't), and release code is unchanged; dead-code elimination keeps described locals, and
   `numrep` moves a description to the narrowed local.
2. **Cranelift emits them.** A described scalar stays in a register for its reads, and each
   assignment also stores it to a shadow stack slot; aggregates live in stack slots anyway
   (`StackSlotKey` finds the slots after compilation). Cranelift's own value locations
   (`set_val_label`) were tried and dropped: at this optimization level they lose most values
   between uses, so a debugger showed `w`, `h` and loop counters as unavailable. A value set from
   the debugger (`expr x = 100`) goes to the slot, which the program does not read back: it keeps
   using the old value. Each becomes a
   `DW_TAG_variable` / `DW_TAG_formal_parameter` with a frame-base location (plus `DW_OP_deref`
   when held by reference) and a DWARF type built from its debug type: base types, structures
   with named members, pointers to class objects, enumerations.
3. **LLDB formatters** (`share/velt/lldb/velt_lldb.py`, loaded by the VS Code extension) show
   strings as text, arrays as their elements, options as `null` or the value and tagged types
   as their active variant, matching debug type names. A value that cannot be one (a variable
   not initialized yet) shows as `<invalid string>` or `<invalid array>`, never read as a huge
   array. They match type names and need no recognizer functions, so they work with LLDB 18.
4. **Scopes.** A variable is in scope from its declaration's line to the end of the block that
   declares it (`LocalDebug::end`; the function's end for its own block and params). Those
   source ranges nest like the blocks, so Cranelift puts each variable in a
   `DW_TAG_lexical_block` inside the one of the innermost variable whose range holds its own,
   covering the code located in its range. Not-yet-declared variables and those of a finished
   loop are hidden.

Locals of async functions, generators and inlined code are not described yet (phase 4: their
values live in state machines or the caller's frame).

## Alternatives

- Field names in a side table indexed by `AggId`: needs no new types, but tagged types, options
  and the source spelling of a type still need a description, and the side table would describe
  layouts rather than source types.
- Scopes as statement ranges: after CFG lowering a scope is not a contiguous range.
- A scope id in every statement location (`SrcLoc`): exact, but it touches every location and
  every pass that makes one; a source range per variable needs neither, since the code of a
  block is the code located inside it.
