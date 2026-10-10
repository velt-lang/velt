// Node version of set_index_keys.vlt (plain objects).
function fill(named) {
  const t0 = performance.now();
  const o = {};
  if (named) o.name = "x";
  for (let i = 0; i < 20000; i++) o[`${i}`] = i;
  const ms = performance.now() - t0;
  console.log(named ? "after a name:" : "alone:", JSON.stringify(o).length);
  return ms;
}
const a = fill(false);
const b = fill(true);
console.error(`set_index_keys alone ${a.toFixed(1)} ms, after a name ${b.toFixed(1)} ms`);
