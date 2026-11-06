// Reads back what encode.velt wrote with Node's zlib and the system's gzip, and prints the sizes
// against Node's own at the same level. Usage: node check.mjs <corpus> <encoded>.
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import zlib from "node:zlib";

const [corpus, encoded] = process.argv.slice(2);
const totals = { 1: [0, 0, 0], 6: [0, 0, 0], 9: [0, 0, 0] };
const rows = [];
let checked = 0;
let wrong = 0;
for (const name of readdirSync(corpus).sort()) {
  const input = readFileSync(`${corpus}/${name}`);
  for (const level of [1, 6, 9]) {
    const read = (kind) => readFileSync(`${encoded}/${name}.${level}.${kind}`);
    const outs = [
      [read("gzip"), zlib.gunzipSync],
      [read("zlib"), zlib.inflateSync],
      [read("raw"), zlib.inflateRawSync],
      [read("pieces"), zlib.gunzipSync],
      [read("flushed"), zlib.gunzipSync],
      [read("gzip"), (b) => execFileSync("gzip", ["-dc"], { input: b, maxBuffer: 1 << 26 })],
      [read("pieces"), (b) => execFileSync("gzip", ["-dc"], { input: b, maxBuffer: 1 << 26 })],
    ];
    for (const [bytes, decode] of outs) {
      checked++;
      if (!decode(bytes).equals(input)) {
        console.log(`differs: ${name} level ${level}`);
        wrong++;
      }
    }
    const ours = read("gzip").length;
    const node = zlib.gzipSync(input, { level }).length;
    totals[level][0] += input.length;
    totals[level][1] += ours;
    totals[level][2] += node;
    if (input.length >= 1 << 20 || name.startsWith("mixed") || name.startsWith("far")) {
      rows.push(`${name.padEnd(22)} ${String(level).padEnd(5)} ${String(input.length).padStart(8)} ${String(ours).padStart(8)} ${String(node).padStart(8)} ${(ours / node).toFixed(3)}`);
    }
  }
}
console.log(`node and gzip: ${checked} decodes, ${wrong} wrong`);
console.log(`file                   level    input     velt     node velt/node`);
console.log(rows.join("\n"));
for (const level of [1, 6, 9]) {
  const [input, ours, node] = totals[level];
  console.log(`all files level ${level}: input ${input}, velt ${ours}, node ${node}, velt/node ${(ours / node).toFixed(3)}`);
}
process.exit(wrong > 0 ? 1 : 0);
