// Navigation (see navigate.vlt): property and index access on parsed objects.
const { doc } = require("./doc.js");
const v = JSON.parse(doc(100000));
const n = v.length;
let sum = 0;
for (let round = 0; round < 20; round++) {
  for (let i = 0; i < n; i++) {
    const item = v[i];
    sum += item.id + Math.trunc(item.score * 2) + item.tags[0].length;
  }
}
console.log(n, sum);
