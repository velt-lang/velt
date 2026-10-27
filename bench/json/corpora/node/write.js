const fs = require("fs");
const [file, rounds] = process.argv.slice(2);
const v = JSON.parse(fs.readFileSync(file, "utf8"));
let out = "";
for (let r = 0; r < Number(rounds); r++) {
  out = JSON.stringify(v);
}
console.log(out);
