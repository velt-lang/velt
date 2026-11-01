// Runs many.wasm: each `next` settles on the next turn of node's event loop. Checks that the
// wasm stack pointer is where it was and memory did not grow between `probe("start")` and
// `probe("end")`: `node many.mjs <velt_web.mjs> <many.wasm>`.

import { readFile } from "node:fs/promises";
import { writeSync } from "node:fs";
import { pathToFileURL } from "node:url";

const [glue, wasm] = process.argv.slice(2);
const { loadVelt } = await import(pathToFileURL(glue).href);
const seen = {};
let rt;
const test = {
  next: (n) => new Promise((resolve) => setImmediate(() => resolve(n))),
  probe: (label) => {
    seen[label] = [rt.exports.__stack_pointer.value, rt.memory.buffer.byteLength];
  },
};
rt = await loadVelt(await readFile(wasm), {
  write: (stream, data) => writeSync(stream, data),
  imports: { test },
});
rt.enter(() => rt.exports.velt_start());
const code = await rt.done;
const [sp0, mem0] = seen.start;
const [sp1, mem1] = seen.end;
console.log(`exit ${code}, stack pointer ${sp1 === sp0 ? "kept" : `moved by ${sp1 - sp0}`}, memory ${mem1 === mem0 ? "kept" : `grew by ${mem1 - mem0}`}`);
