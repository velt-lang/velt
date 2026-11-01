# velt/sync/epoch

Epoch-based reclamation, written in Velt on `Atomic`, `ThreadLocal` and `Mutex`: a way to drop
a `shared` value that other threads may still be reading, once none can be. The compiler calls
it for the values an `Atomic` of a `shared` value replaces, and opens a section around a
function that loads one. Lock-free code of your own can call it too.

- `pin()` enters a *section*, and `unpin()` leaves it. Sections nest; an `unpin()` without a
  `pin()` before it on the thread is an error. While a thread is in one,
  no value retired since it entered, nor one retired before that it can still reach, is
  dropped. Entering costs a store and a fence, and leaving a store. Keep a section between two
  `await`s: a section belongs to the thread it was entered on, and a task may resume on another
  thread after an `await`, where its `unpin()` is an error (and the first thread stays pinned).
- `retire(v)` hands over a `shared` value just taken out of where other threads find it. It is
  dropped once every thread that was in a section then has left it.
- `collect()` moves the global epoch on if every thread in a section has seen it, then drops
  what was retired two epochs or more before, on every thread's list. It returns how many it
  dropped. `retire` calls it every 64 retirements.
- `pending()` counts this thread's values not dropped yet, `outstanding()` every thread's, and
  `dropped()` all values dropped so far.

```ts
import { pin, unpin, retire, collect, pending } from "velt/sync/epoch";

interface Item {
  n: u64;
}

function main() {
  retire(shared({ n: 1 }));
  pin();
  console.log(collect(), pending()); // 0 1: this thread may still read it
  unpin();
  console.log(collect(), pending()); // 1 0
}
```
