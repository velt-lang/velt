// Node twin of tests/golden/std/fs_whole_files.velt: the same calls through node:fs, for the
// golden's expected output.
import * as fs from "node:fs";

function attempt(what, f) {
  try {
    f();
    console.log(what, "no error");
  } catch (e) {
    if (e.code !== undefined && e.syscall !== undefined) {
      console.log(what, e.code, e.message);
    } else {
      console.log(what, e.message);
    }
  }
}

async function main() {
  const dir = "velt_fs_whole_files_tmp";
  if (fs.existsSync(dir)) {
    fs.rmSync(dir, { recursive: true });
  }
  fs.mkdirSync(dir, { recursive: true });
  const a = `${dir}/a.txt`;

  fs.writeFileSync(a, "first");
  fs.writeFileSync(a, "héllo");
  fs.appendFileSync(a, " wörld\n");
  console.log(JSON.stringify(fs.readFileSync(a, "utf8")), fs.readFileSync(a).length);
  fs.appendFileSync(`${dir}/made.txt`, "made by append");
  console.log(fs.readFileSync(`${dir}/made.txt`, "utf8"));

  fs.writeFileSync(`${dir}/empty.txt`, "");
  console.log(JSON.stringify(fs.readFileSync(`${dir}/empty.txt`, "utf8")), fs.readFileSync(`${dir}/empty.txt`).length);


  const big = "0123456789abcdef".repeat(65536 + 3);
  fs.writeFileSync(`${dir}/big.txt`, big);
  const back = fs.readFileSync(`${dir}/big.txt`, "utf8");
  console.log(back.length, back === big);

  const s = fs.statSync(a);
  console.log(s.size, s.isFile(), s.isDirectory(), s.isSymbolicLink(), s.mtimeMs > 1.6e12);
  const d = fs.statSync(dir);
  console.log(d.isFile(), d.isDirectory(), fs.lstatSync(a).isFile());

  console.log(fs.existsSync(a), fs.existsSync(dir), fs.existsSync(`${dir}/none`), fs.existsSync(`${dir}/a\0b`));

  attempt("read missing", () => fs.readFileSync(`${dir}/none.txt`, "utf8"));
  attempt("bytes missing", () => fs.readFileSync(`${dir}/none.txt`));
  attempt("write into missing", () => fs.writeFileSync(`${dir}/no/such.txt`, "x"));
  attempt("append into missing", () => fs.appendFileSync(`${dir}/no/such.txt`, "x"));
  attempt("stat missing", () => fs.statSync(`${dir}/none.txt`));
  attempt("lstat missing", () => fs.lstatSync(`${dir}/none.txt`));

  await fs.promises.writeFile(`${dir}/b.txt`, "async");
  await fs.promises.appendFile(`${dir}/b.txt`, " too");
  console.log(await fs.promises.readFile(`${dir}/b.txt`, "utf8"), (await fs.promises.readFile(`${dir}/b.txt`)).length);
  console.log((await fs.promises.stat(`${dir}/b.txt`)).size, (await fs.promises.lstat(dir)).isDirectory());
  const there = async (p) => fs.promises.access(p).then(() => true, () => false);
  console.log(await there(`${dir}/b.txt`), await there(`${dir}/none`));
  try {
    await fs.promises.readFile(`${dir}/none.txt`, "utf8");
  } catch (e) {
    console.log("async read missing", e.code, e.message);
  }

  fs.rmSync(dir, { recursive: true });
}

main();
