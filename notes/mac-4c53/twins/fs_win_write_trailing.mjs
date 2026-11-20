// Node twin of tests/golden/std/fs_win_write_trailing.velt. Run on Windows in an empty directory
// with Node 24: node fs_win_write_trailing.mjs > fs_win_write_trailing.out
import * as fs from "node:fs";

function attempt(what, f) {
  try {
    f();
    console.log(what, "ok");
  } catch (e) {
    console.log(what, e.code);
  }
}

const dir = "velt_fs_win_write_trailing_tmp";
if (fs.existsSync(dir)) {
  fs.rmSync(dir, { recursive: true });
}
fs.mkdirSync(`${dir}/d`, { recursive: true });
fs.writeFileSync(`${dir}/f`, "one");
attempt("writeFile f/", () => fs.writeFileSync(`${dir}/f/`, "two"));
attempt("appendFile f\\", () => fs.appendFileSync(`${dir}\\f\\`, "+three"));
console.log("f", fs.readFileSync(`${dir}/f`, "utf8"));
attempt("writeFile new/", () => fs.writeFileSync(`${dir}/new/`, "made"));
attempt("appendFile newer//", () => fs.appendFileSync(`${dir}/newer//`, "made too"));
console.log("new", fs.readFileSync(`${dir}/new`, "utf8"), fs.readFileSync(`${dir}/newer`, "utf8"));
attempt("writeFile d/", () => fs.writeFileSync(`${dir}/d/`, "x"));
attempt("appendFile d/", () => fs.appendFileSync(`${dir}/d/`, "x"));
attempt("writeFile d\\", () => fs.writeFileSync(`${dir}\\d\\`, "x"));
attempt("appendFile d/\\", () => fs.appendFileSync(`${dir}/d/\\`, "x"));
attempt("writeFile missing/x/", () => fs.writeFileSync(`${dir}/missing/x/`, "x"));
console.log(fs.readdirSync(dir).sort().join(" "));
fs.rmSync(dir, { recursive: true });
