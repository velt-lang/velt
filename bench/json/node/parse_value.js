// Dynamic parse (see parse_value.velt): `JSON.parse` into plain objects.
const { doc } = require("./doc.js");
const text = doc(100000);
let sum = 0;
for (let round = 0; round < 8; round++) {
  const v = JSON.parse(text);
  sum += v.length + Object.keys(v[round]).length;
}
console.log(text.length, sum);
