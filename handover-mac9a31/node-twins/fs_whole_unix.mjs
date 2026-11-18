// Node twin of tests/golden/std/fs_whole_unix.velt.
import * as fs from "node:fs";

function attempt(what, f) {
  try {
    f();
    console.log(what, "no error");
  } catch (e) {
    console.log(what, e.code, e.message);
  }
}

const dir = "velt_fs_whole_unix_tmp";
if (fs.existsSync(dir)) {
  fs.rmSync(dir, { recursive: true });
}
fs.mkdirSync(dir, { recursive: true });
const a = `${dir}/a.txt`;
fs.writeFileSync(a, "a");
attempt("read under a file", () => fs.readFileSync(`${a}/x`, "utf8"));
attempt("write under a file", () => fs.writeFileSync(`${a}/x`, "x"));
attempt("stat under a file", () => fs.statSync(`${a}/x`));
const fd = fs.openSync(`${dir}/bad.txt`, "w");
fs.writeSync(fd, new Uint8Array([0x61, 0xff, 0x62, 0xe2, 0x82]));
fs.closeSync(fd);
console.log(JSON.stringify(fs.readFileSync(`${dir}/bad.txt`, "utf8")), fs.readFileSync(`${dir}/bad.txt`).length);
fs.rmSync(dir, { recursive: true });
