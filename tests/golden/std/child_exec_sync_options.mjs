import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import { constants, platform, tmpdir } from "node:os";

function hide(text, dir) {
  return text.split(dir).join("<dir>");
}

function count(text, c) {
  let other = 0;
  for (const ch of text) {
    if (ch != c) {
      other++;
    }
  }
  return `${text.length} bytes, ${other} other`;
}

// Velt's `execSync` as Node's `spawnSync`: the exit code, or 128 + the signal that killed it.
function execSync(program, args, opts = {}) {
  const r = spawnSync(program, args, { encoding: "utf8", maxBuffer: 1 << 30, ...opts });
  const code = r.status ?? 128 + constants.signals[r.signal];
  return { code, stdout: r.stdout, stderr: r.stderr };
}

function run(label, program, args, opts, dir) {
  const r = execSync(program, args, opts);
  console.log(`${label}: ${r.code} [${hide(r.stdout, dir).trim()}] [${r.stderr.trim()}]`);
}

function failure(program, opts) {
  const r = spawnSync(program, [], opts);
  return r.error ? r.error.code : "ran";
}

const LINUX = platform() == "linux";

function macOnly(label, got) {
  console.log(`${label}: checked on macOS`);
  if (!LINUX) {
    const g = got();
    if (g != "ENAMETOOLONG") {
      console.log("  got:", g);
    }
  }
}

function main() {
  const dir = `${tmpdir()}/velt-golden-exec-sync-options-${process.pid}`;
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(`${dir}/marker`, "");

  run("input", "cat", [], { input: "hello\nworld\n" }, dir);
  run("no input", "cat", [], {}, dir);
  run("cwd", "ls", [], { cwd: dir }, dir);
  const script = 'echo "foo=$VELT_GOLDEN_FOO home=${HOME:+set}"';
  run("env", "sh", ["-c", script], { env: { ...process.env, VELT_GOLDEN_FOO: "bar" } }, dir);
  run("env alone", "sh", ["-c", script], { env: { VELT_GOLDEN_FOO: "bar" } }, dir);
  run("exit", "sh", ["-c", "echo out; echo err >&2; exit 7"], {}, dir);
  run("signal", "sh", ["-c", "kill -9 $$"], {}, dir);
  const long = `${("/" + "b".repeat(99)).repeat(50)}:/usr/bin:/bin`;
  run("long PATH entry", "sh", ["-c", "echo found"], { env: { PATH: long } }, dir);
  fs.mkdirSync(`${dir}/bin`, { recursive: true });
  fs.writeFileSync(`${dir}/bin/prog`, "#!/bin/sh\necho bin\n", { mode: 0o755 });
  fs.writeFileSync(`${dir}/prog`, "#!/bin/sh\necho cwd\n", { mode: 0o755 });
  const cwdTry = { PATH: `/${"x".repeat(4999)}:${dir}/bin` };
  const ran = spawnSync("prog", [], { cwd: dir, env: cwdTry, encoding: "utf8" }).stdout.trim();
  console.log("long entry then a dir as Node:", ran == (LINUX ? "cwd" : "bin"));
  const longDir = `/${"d".repeat(299)}:/usr/bin:/bin`;
  macOnly("long dir is ENAMETOOLONG", () => failure("sh", { env: { PATH: longDir } }));
  const nope = { PATH: "/nope1:/nope2" };
  macOnly("long name is ENAMETOOLONG", () => failure("p".repeat(256), { env: nope }));
  console.log("255-byte name:", failure("p".repeat(255), { env: nope }));

  const both = "yes o | head -c 300000; yes e | head -c 300000 >&2; yes o | head -c 300000";
  const big = execSync("sh", ["-c", both]);
  const out = big.stdout.replaceAll("\n", "");
  const err = big.stderr.replaceAll("\n", "");
  console.log(`both pipes: ${big.code} stdout ${count(out, "o")}, stderr ${count(err, "e")}`);

  const first = execSync("sh", ["-c", "yes w | head -c 300000; wc -c"], { input: "i".repeat(300000) });
  const lines = first.stdout.trim().split("\n");
  console.log(`input after output: ${first.code} ${lines.length} lines, read ${lines[lines.length - 1].trim()}`);

  fs.rmSync(dir, { recursive: true });
}

main();
