// Runs a program built from main.vlt (or another program of this directory) with async host
// functions, and prints its exit code: `node host.mjs <velt_web.mjs> <program.wasm> [how]` (what
// `how()` returns to ends.vlt). A host interval counts the ticks of node's event loop, which
// runs while the program waits.

import { readFile } from "node:fs/promises";
import { writeSync } from "node:fs";
import { pathToFileURL } from "node:url";

const [glue, wasm, how = "return"] = process.argv.slice(2);
const { runVelt, HostRefs } = await import(pathToFileURL(glue).href);
const refs = new HostRefs();
const boxes = new Map();
const gates = new Map();
let ticks = 0;
const interval = setInterval(() => ticks++, 1);
const soon = (v) => new Promise((resolve) => setTimeout(() => resolve(v), 1));
const test = {
  num: (n) => soon(n * 2),
  big: async (n) => n + 1n,
  real: () => 1.5,
  yes: async () => true,
  small: async () => -3,
  tooBig: async () => 300,
  text: async (s) => `hello, ${s}!`,
  bytes: async (n) => Uint8Array.from({ length: n }, (_, i) => i * 10),
  box: async (label) => {
    if (label === "") return null;
    const b = { label };
    boxes.set(label, b);
    return b;
  },
  find: async (label) => boxes.get(label) ?? null,
  label: async (b) => b.label,
  nothing: () => soon(undefined),
  fail: async (name) => {
    if (name === "plain") throw "a plain value";
    throw new TypeError(`no ${name}`);
  },
  gate: (id) => new Promise((resolve) => gates.set(id, resolve)),
  open: (ids) => {
    for (const id of ids.split(",")) gates.get(Number(id))(Number(id) * 100);
  },
  how: async () => how,
  ticks: () => ticks,
  live: () => refs.values.size,
};
// The program's stderr goes to stdout too, so the expected output shows it in its place.
const write = (stream, data) => writeSync(1, data);
const code = await runVelt(await readFile(wasm), { write, imports: { test }, refs });
clearInterval(interval);
console.log(`exit ${code}, ${refs.values.size} values held`);
