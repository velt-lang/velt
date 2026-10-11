// Node version of record_reads.vlt.
function decoded() {
  const parts = [];
  for (let i = 0; i < 20000; i++) parts.push(`"k${i}":${i}`);
  const r = JSON.parse(`{${parts.join(",")}}`);
  const t0 = performance.now();
  let n = 0;
  for (let j = 0; j < 200; j++) n += Object.keys(r).length + Object.values(r).length;
  const ms = performance.now() - t0;
  console.log("decoded:", n);
  return ms;
}
function dates() {
  const r = {};
  for (let i = 0; i < 20000; i++) r[`2024-01-${i}`] = i;
  const t0 = performance.now();
  let n = 0;
  for (let j = 0; j < 200; j++) n += Object.values(r).length;
  const ms = performance.now() - t0;
  console.log("dates:", n);
  return ms;
}
const a = decoded();
const b = dates();
console.error(`record_reads decoded ${a.toFixed(1)} ms, dates ${b.toFixed(1)} ms`);
