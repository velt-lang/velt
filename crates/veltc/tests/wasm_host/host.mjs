// Runs main.wasm (built from main.vlt) with the host functions it declares, then checks that
// the program released every HostRef and that a missing host function is an error before it
// runs: `node host.mjs <velt_web.mjs> <main.wasm>`.

import { readFile } from "node:fs/promises";
import { writeSync } from "node:fs";
import { pathToFileURL } from "node:url";

const [glue, wasm] = process.argv.slice(2);
const { runVelt, HostRefs } = await import(pathToFileURL(glue).href);
const bytes = await readFile(wasm);
const refs = new HostRefs();
const boxes = new Map();
const test = {
  add: (a, b, big) => a + b + Number(big),
  flag: (b) => !b,
  greet: (name) => `hello, ${name}!`,
  fill: (buf, value) => {
    buf.fill(value);
    return buf.length;
  },
  sum: (data) => data.reduce((a, b) => a + b, 0),
  reversed: (data) => data.reverse(),
  makeBox: (label) => {
    const box = { label };
    boxes.set(label, box);
    return box;
  },
  find: (label) => boxes.get(label),
  label: (box) => box.label,
  same: (a, b) => a === b,
  live: () => refs.values.size,
  small: () => 200,
  tiny: () => -128,
  wide: () => -(2n ** 63n),
  wideU: () => 2 ** 53,
  word: () => 0xffffffff,
  echo: (data) => data,
  echoTail: (data) => data.subarray(1),
};
const write = (stream, data) => writeSync(stream, data);
const code = await runVelt(bytes, { write, imports: { test }, refs });
console.log(`exit ${code}, ${refs.values.size} values held`);
// What the host returns must fit the declared type: anything else stops the program (its output
// before then is dropped here).
const bad = [
  ["small", 300],
  ["small", -1],
  ["small", 1.5],
  ["small", "7"],
  ["small", undefined],
  ["tiny", 128],
  ["word", -1],
  ["word", 2 ** 32],
  ["wide", 2n ** 63n],
  ["wide", 1.5],
  ["wide", "12"],
  ["wideU", -1],
  ["wideU", 2n ** 64n],
  ["add", "1"],
  ["flag", 1],
  ["greet", 7],
  ["reversed", [1, 2]],
  ["makeBox", null],
];
for (const [name, value] of bad) {
  try {
    const imports = { test: { ...test, [name]: () => value } };
    await runVelt(bytes, { write: () => {}, imports });
  } catch (e) {
    console.log(`${e.name}: ${e.message}`);
  }
}
try {
  await runVelt(bytes, { write, imports: { test: {} } });
} catch (e) {
  console.log(e.message);
}
