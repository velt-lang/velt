// Node's side of bench/json/fuzz/values.vlt.
const fs = require("fs");
function keysOf(v, out) {
  if (v !== null && typeof v === "object") {
    if (!Array.isArray(v)) {
      out.push(JSON.stringify(Object.keys(v)));
    }
    for (const x of Object.values(v)) {
      keysOf(x, out);
    }
  }
}
for (const line of fs.readFileSync("values.txt", "utf8").split("\n")) {
  if (line.length == 0) {
    continue;
  }
  const v = JSON.parse(line);
  const keys = [];
  keysOf(v, keys);
  console.log(JSON.stringify(v), keys.join(" "));
}
