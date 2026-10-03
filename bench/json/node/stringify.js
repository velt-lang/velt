// Stringify (see stringify.vlt).
const items = [];
for (let i = 0; i < 100000; i++) {
  items.push({ id: i, name: `user ${i}`, score: (i % 1000) + 0.5, active: i % 2 == 0, tags: [`t${i % 7}`, "x"] });
}
let total = 0;
for (let round = 0; round < 8; round++) {
  total += JSON.stringify(items).length;
}
console.log(total);
