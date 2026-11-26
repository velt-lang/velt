import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import { tmpdir } from "node:os";

function lines(text) {
  const all = text.trim().split("\n");
  return `${all.length} lines, ${all[0].trim()} .. ${all[all.length - 1].trim()}`;
}

// Velt's `execSync` as Node's `spawnSync`: the exit code and both outputs as text. Input the child
// never reads makes Node report `error` EOF beside the result; Velt drops the rest and runs on.
function execSync(program, args, opts = {}) {
  const r = spawnSync(program, args, { encoding: "utf8", maxBuffer: 1 << 30, ...opts });
  if (r.error && r.status === null) {
    throw r.error;
  }
  return { code: r.status, stdout: r.stdout, stderr: r.stderr };
}

function cmd(line, opts = {}) {
  return execSync("cmd.exe", ["/d", "/c", line], opts);
}

function failure(program) {
  const r = spawnSync(program, [], {});
  return r.error ? r.error.code : "ran";
}

const both = cmd("for /l %i in (1,1,12000) do @(echo out %i & echo err %i 1>&2)");
console.log(`both: ${both.code}; stdout ${lines(both.stdout)}; stderr ${lines(both.stderr)}`);

let input = "";
for (let i = 1; i <= 30000; i++) {
  input += `in ${i}\n`;
}
const echoed = execSync("findstr.exe", ["^"], { input: input });
console.log(`echoed: ${echoed.code}; ${echoed.stdout.length} bytes; ${lines(echoed.stdout)}`);

const unread = cmd("echo not reading", { input: input });
console.log(`unread: ${unread.code} [${unread.stdout.trim()}]`);

const empty = execSync("findstr.exe", ["^"], {});
console.log(`no input: ${empty.code} [${empty.stdout}]`);

const exit3 = cmd("echo to out & echo to err 1>&2 & exit 3");
console.log(`exit 3: ${exit3.code} [${exit3.stdout.trim()}] [${exit3.stderr.trim()}]`);

const dir = `${tmpdir()}\\velt-golden-exec-sync-win`;
fs.mkdirSync(dir, { recursive: true });
const where = cmd("cd & echo %VELT_GOLDEN_VAR%", {
  cwd: dir,
  env: { ...process.env, VELT_GOLDEN_VAR: "value of the variable" },
});
console.log(`cwd and env: ${where.code} [${where.stdout.trim().split(dir).join("<dir>")}]`);
fs.rmdirSync(dir);

console.log(`missing: ${failure("velt-golden-no-such-program")}`);
console.log(`batch: ${failure("velt-golden-script.bat")}`);
