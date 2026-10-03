// Typed parse (see parse_typed.vlt): JavaScript has no typed parse, so `JSON.parse`.
const { doc } = require("./doc.js");
const text = doc(100000);
let sum = 0;
for (let round = 0; round < 8; round++) {
  const items = JSON.parse(text);
  for (const it of items) {
    sum += it.id + Math.trunc(it.score * 2) + (it.active ? 1 : 0);
    sum += it.name.length + it.tags[0].length;
  }
}
console.log(text.length, sum);
