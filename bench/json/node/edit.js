// Editing (see edit.vlt): add 10k keys to an object, then `delete` them from the front.
const n = 10000;
const keys = [];
for (let i = 0; i < n; i++) {
  keys.push(`key${i}`);
}
let sum = 0;
for (let round = 0; round < 4; round++) {
  const obj = {};
  for (let i = 0; i < n; i++) {
    obj[keys[i]] = i;
  }
  sum += Object.keys(obj).length;
  for (const k of keys) {
    if (k in obj) {
      delete obj[k];
      sum += 1;
    }
  }
  sum += Object.keys(obj).length;
}
console.log(sum);
