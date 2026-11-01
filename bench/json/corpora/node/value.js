const fs = require("fs");
const [file, rounds] = process.argv.slice(2);
const text = fs.readFileSync(file, "utf8");
let n = 0;
for (let r = 0; r < Number(rounds); r++) {
  const v = JSON.parse(text);
  n = Array.isArray(v) ? v.length : Object.keys(v).length;
}
console.log(n);
