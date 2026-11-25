import { execFile, spawn, spawnSync } from "node:child_process";
import * as fs from "node:fs";
import { platform, tmpdir } from "node:os";

const LINUX = platform() == "linux";

function hide(text, dir) {
  return text.split(dir).join("<dir>");
}

function ran(code, stdout, stderr, dir) {
  const err = stderr.trim();
  return hide(`ran ${code} ${stdout.trim()}${err == "" ? "" : ` ${err}`}`, dir);
}

function failed(err, dir) {
  const args = err.spawnargs == null ? "none" : JSON.stringify(err.spawnargs);
  const fields = `${err.code} ${err.errno} ${err.syscall} ${err.path ?? "none"} ${args}`;
  return hide(`error ${fields} | ${err.message}`, dir);
}

function check(label, got, want) {
  console.log(label, got == want);
  if (got != want) {
    console.log("  got: ", got);
    console.log("  want:", want);
  }
}

function syncThrown(file, args) {
  const fields = `ENOEXEC -8 spawnSync ${file} ${file} ${JSON.stringify(args)}`;
  return `error ${fields} | spawnSync ${file} ENOEXEC`;
}

function sync(file, args, opts, dir) {
  const r = spawnSync(file, args, { encoding: "utf8", ...opts });
  return r.error ? failed(r.error, dir) : ran(r.status, r.stdout, r.stderr, dir);
}

function run(file, args, opts, dir) {
  return new Promise((resolve) => {
    try {
      execFile(file, args, opts, (err, stdout, stderr) => {
        if (err && typeof err.code !== "number") {
          resolve(failed(err, dir));
        } else {
          resolve(ran(err ? err.code : 0, stdout, stderr, dir));
        }
      });
    } catch (err) {
      resolve(failed(err, dir));
    }
  });
}

async function main() {
  const dir = `${tmpdir()}/velt-golden-child-no-shebang-${process.pid}`;
  fs.mkdirSync(`${dir}/bin`, { recursive: true });
  const body = 'echo "zero=$0 count=$# args=$*"\necho "to stderr" >&2\nexit 3\n';
  fs.writeFileSync(`${dir}/bin/script`, body, { mode: 0o755 });
  const path = { ...process.env, PATH: "bin" };
  const thrown = "error ENOEXEC -8 spawn none none | spawn ENOEXEC";

  // Node's spawnSync (Velt's execSync).
  let got = sync("./bin/script", ["a", "b c"], { cwd: dir }, dir);
  const syncWant = "ran 3 zero=./bin/script count=2 args=a b c to stderr";
  check("execSync as Node:", got, LINUX ? syncWant : syncThrown("./bin/script", ["a", "b c"]));
  got = sync(`${dir}/bin/script`, ["x"], {}, dir);
  check(
    "execSync absolute as Node:",
    got,
    LINUX
      ? "ran 3 zero=<dir>/bin/script count=1 args=x to stderr"
      : syncThrown("<dir>/bin/script", ["x"]),
  );
  got = sync("script", ["y"], { cwd: dir, env: path }, dir);
  check(
    "execSync in PATH as Node:",
    got,
    LINUX ? "ran 3 zero=bin/script count=1 args=y to stderr" : syncThrown("script", ["y"]),
  );

  got = await run("./bin/script", ["a", "b c"], { cwd: dir }, dir);
  const execWant = "ran 3 zero=./bin/script count=2 args=a b c to stderr";
  check("exec as Node:", got, LINUX ? execWant : thrown);
  got = await run("script", ["y"], { cwd: dir, env: path }, dir);
  check("exec in PATH as Node:", got, LINUX ? "ran 3 zero=bin/script count=1 args=y to stderr" : thrown);

  try {
    const p = spawn("./bin/script", ["q"], { cwd: dir, stdio: ["ignore", "pipe", "ignore"] });
    let out = "";
    p.stdout.on("data", (d) => (out += d));
    const code = await new Promise((resolve) => p.on("close", resolve));
    got = ran(code, out, "", dir);
  } catch (err) {
    got = failed(err, dir);
  }
  check("spawn as Node:", got, LINUX ? "ran 3 zero=./bin/script count=1 args=q" : thrown);

  fs.rmSync(dir, { recursive: true });
}

main();
