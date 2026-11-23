// Building and writing (see build_write.velt): 3 rounds of 20k records built, read back, copied with
// every fourth record changed, and both arrays written.
const n = 20000;
let total = 0;
for (let round = 0; round < 3; round++) {
  const rows = [];
  for (let i = 0; i < n; i++) {
    rows.push({ id: i, name: `user ${i}`, score: (i % 1000) + 0.5, active: i % 2 == 0, tags: [`t${i % 7}`, "x"], note: null });
  }
  let sum = 0;
  for (let i = 0; i < n; i++) {
    sum += rows[i].id + rows[i].tags[0].length;
  }
  const copy = rows.map((r, i) => (i % 4 == 0 ? { ...r, score: 0 } : r));
  total += JSON.stringify(rows).length + JSON.stringify(copy).length + sum;
}
console.log(total);
