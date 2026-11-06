// Reads back what brotli_encode.velt wrote with Node's Brotli decoder, and prints the sizes
// against Node's encoder at the same quality. Usage: node brotli_check.mjs <corpus> <encoded>.
import { readFileSync, readdirSync } from "node:fs";
import zlib from "node:zlib";

const [corpus, encoded] = process.argv.slice(2);
const QUALITIES = [0, 1, 4, 6, 9, 11];
const totals = Object.fromEntries(QUALITIES.map((q) => [q, [0, 0, 0]]));
const rows = [];
let checked = 0;
let wrong = 0;
for (const name of readdirSync(corpus).sort()) {
  const input = readFileSync(`${corpus}/${name}`);
  for (const q of QUALITIES) {
    for (const kind of ["br", "pieces", "flushed"]) {
      const bytes = readFileSync(`${encoded}/${name}.${q}.${kind}`);
      checked++;
      let out = null;
      try {
        out = zlib.brotliDecompressSync(bytes);
      } catch (e) {
        console.log(`node: ${e.message}: ${name} quality ${q} ${kind}`);
      }
      if (out == null || !out.equals(input)) {
        console.log(`differs: ${name} quality ${q} ${kind}`);
        wrong++;
      }
    }
    const ours = readFileSync(`${encoded}/${name}.${q}.br`).length;
    const node = zlib.brotliCompressSync(input, {
      params: { [zlib.constants.BROTLI_PARAM_QUALITY]: q, [zlib.constants.BROTLI_PARAM_SIZE_HINT]: input.length },
    }).length;
    totals[q][0] += input.length;
    totals[q][1] += ours;
    totals[q][2] += node;
    if (input.length >= 1 << 20 || name.startsWith("mixed") || name.startsWith("far")) {
      rows.push(`${name.padEnd(22)} ${String(q).padEnd(3)} ${String(input.length).padStart(8)} ${String(ours).padStart(8)} ${String(node).padStart(8)} ${(ours / node).toFixed(3)}`);
    }
  }
}
console.log(`node: ${checked} decodes, ${wrong} wrong`);
console.log(`file                   q      input     velt     node velt/node`);
console.log(rows.join("\n"));
for (const q of QUALITIES) {
  const [input, ours, node] = totals[q];
  console.log(`all files quality ${q}: input ${input}, velt ${ours}, node ${node}, velt/node ${(ours / node).toFixed(3)}`);
}
process.exit(wrong > 0 ? 1 : 0);
