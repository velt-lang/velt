// The glue's table of `HostRef`s over a long-lived instance (crates/veltc/tests/workers.rs):
// `node refs.mjs <velt_web.mjs>`. Released slots are used again, so slots stay small however
// many values come and go, and a released slot misses even once its index holds another value.
import { pathToFileURL } from "node:url";

const { HostRefs } = await import(pathToFileURL(process.argv[2]).href);
const refs = new HostRefs();
const kept = refs.add("kept");
let highest = 0;
let stale = 0;
for (let i = 0; i < 100_000; i++) {
  const a = refs.add({ i });
  const b = refs.add({ i });
  highest = Math.max(highest, a & 0xfffff, b & 0xfffff);
  refs.release(a);
  refs.release(b);
  if (i === 0) stale = a;
}
const reused = refs.add("new");
let missed = false;
try {
  refs.get(stale);
} catch {
  missed = true;
}
console.log(`indexes up to ${highest}, ${refs.values.size} held, kept ${refs.get(kept)}`);
console.log(`a released slot misses: ${missed}; its index holds another value: ${(reused & 0xfffff) === (stale & 0xfffff)}`);
console.log(`slots are u32: ${[kept, reused].every((s) => s > 0 && s <= 0xffffffff)}`);
