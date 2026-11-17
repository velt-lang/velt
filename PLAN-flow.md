# crew #34 P2b flow precision (stacked on #3104; not committed)

Goal: sigx `head.ts:56/69/91` (and `:108`, see below) convert, kit 151 -> 147, sound.

A record copy `c` (`Copy::record`, source a record: `object_copies/record_widen.rs`) is refused
only where the program could tell. Today that is type-level: any write to a record of either type,
any write through `any` once a value holding the source type is dynamic, any identity test of the
target type, anywhere in the program. Precision: only what can run while the copy lives, and only
writes that may reach the copy or the source.

## Steps
1. copy_flow: the record copy (a `Block` from `with_temp`, span = conversion span, ty = target)
   is `Store::Copy(i)` (`copy_index` also for blocks). `E::Closure(d)` / `E::FnRef(d)` are sources
   `Store::Fn(d)` (new), so a fn value's reach is computable. Expose `origins(store)` (reverse
   reach) and `reach(store)`.
2. Lifetime: `F(c)` = functions with a `Local(f, _)` in the copy's reach. `Live(c)` = closure of
   `F(c)` over calls: `Callee::Def(d)` -> d (and a constructor for `New`); `Indirect(e)` -> every
   `d` whose `Fn(d)` reach meets `sources(e)`; `Virtual` / `Dyn` / `ParamMethod` -> unresolved.
   Unresolved, an `await` / `yield` in `Live`, the copy reaching the heap or `Ret` of an async fn:
   fall back to the type-level rule.
3. Writes (`record_writes`, typed): find each write's function and receiver (walk bodies for the
   record method call at the span, as crew34-p1-full.patch `record_writes::receivers`). Refuse if
   the function is in `Live(c)` and the receiver's origins meet the copy's reach or the source's
   origins (Heap counts as shared), or the receiver's type is generic and in Live.
4. Dynamic writes (`writes` with `dynamic`): refuse if in `Live(c)` and `dynamic_reaches`.
5. Identity looks (`Seen::Identity|Key` of the target type): refuse if in `Live(c)`.
6. `head.ts:108` also needs the object->Record cast (`Copy::record` of an object) to use the same
   lifetime for its identity rule (directives.ts:155 is not in Live of `serializeAttrs`).
7. Goldens: each fallback (await in Live, indirect call to an unknown fn value, virtual call)
   refuses; a callee writing the source refuses; a reader-only callee chain accepts (head.ts
   shapes). Kit; gate; PR stacked on #3104 (stacking note in a comment, not the description).

## Notes
- object_copies/mod.rs is at 798 lines: new code in object_copies/record_flow.rs; move code out
  of mod.rs first if it needs a field.
- Shared target dir: `cargo clean -p velt_common -p velt_sema -p velt_vir -p veltc` when switching
  worktrees.
