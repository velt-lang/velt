// Node fetch (undici): the four benchmark scenarios.
const base = process.argv[2];
const which = process.argv[3] ?? "all";
async function worker(n) {
  let total = 0;
  for (let i = 0; i < n; i++) {
    const r = await fetch(`${base}/small`);
    total += (await r.text()).length;
  }
  return total;
}
if (which === "all" || which === "seq") {
  const t = performance.now();
  const n = await worker(10000);
  const s = (performance.now() - t) / 1000;
  console.log(`seq: ${Math.round(10000 / s)} req/s (${n} bytes) ${s.toFixed(3)}s`);
}
if (which === "all" || which === "conc") {
  const t = performance.now();
  const rs = await Promise.all(Array.from({ length: 100 }, () => worker(1000)));
  const n = rs.reduce((a, b) => a + b, 0);
  const s = (performance.now() - t) / 1000;
  console.log(`conc: ${Math.round(100000 / s)} req/s (${n} bytes) ${s.toFixed(3)}s`);
}
if (which === "all" || which === "big") {
  const t = performance.now();
  const r = await fetch(`${base}/big`);
  const b = await r.bytes();
  const s = (performance.now() - t) / 1000;
  console.log(`big: ${b.length} bytes ${s.toFixed(3)}s ${Math.round(b.length / 1048576 / s)} MB/s`);
}
if (which === "all" || which === "json") {
  const t = performance.now();
  let n = 0;
  for (let i = 0; i < 20; i++) {
    const r = await fetch(`${base}/json`);
    const users = await r.json();
    n += users.length;
  }
  const s = (performance.now() - t) / 1000;
  console.log(`json: 20 x 1MB ${s.toFixed(3)}s ${(s * 1000 / 20).toFixed(1)} ms each (${n} users)`);
}
