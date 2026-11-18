// Node twin of tests/golden/std/fs_whole_paths.velt.
import * as fs from "node:fs";

function attempt(what, f) {
  try {
    f();
    console.log(what, "no error");
  } catch (e) {
    console.log(what, e.code, e.message);
  }
}

async function main() {
  const dir = "velt_fs_whole_paths_tmp";
  if (fs.existsSync(dir)) {
    fs.rmSync(dir, { recursive: true });
  }
  const deep = `${dir}/${"a".repeat(100)}/${"b".repeat(100)}/${"c".repeat(100)}`;
  fs.mkdirSync(deep, { recursive: true });
  const f = `${deep}/file.txt`;
  console.log(f.length > 260);
  fs.writeFileSync(f, "long ");
  fs.appendFileSync(f, "path");
  console.log(fs.readFileSync(f, "utf8"), fs.readFileSync(f).length, fs.statSync(f).size, fs.existsSync(f));
  await fs.promises.writeFile(`${deep}/async.txt`, "async too");
  console.log(await fs.promises.readFile(`${deep}/async.txt`, "utf8"), (await fs.promises.stat(`${deep}/async.txt`)).isFile());
  console.log(fs.existsSync(`${deep}/none.txt`), fs.statSync(deep).isDirectory());

  attempt("read a directory", () => fs.readFileSync(dir, "utf8"));
  attempt("read a directory/", () => fs.readFileSync(`${dir}/`, "utf8"));
  attempt("bytes of a directory", () => fs.readFileSync(dir));
  try {
    await fs.promises.readFile(dir, "utf8");
  } catch (e) {
    console.log("async read a directory", e.code, e.message);
  }
  fs.rmSync(dir, { recursive: true });
}

main();
