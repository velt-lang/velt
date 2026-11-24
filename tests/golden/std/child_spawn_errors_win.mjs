import { execFile, execFileSync, spawn } from "node:child_process";
import * as fs from "node:fs";
import { tmpdir } from "node:os";

function show(what, err) {
  const args = err.spawnargs == null ? "none" : JSON.stringify(err.spawnargs);
  console.log(what, err.code, err.errno, err.syscall, err.path ?? "none", args);
  console.log("  ", err.message);
}

async function attempt(label, program, vars, cwd) {
  const env = { ...process.env, ...vars };
  // Node's `spawn` throws some errors (EINVAL for a batch file) rather than emitting them.
  try {
    await new Promise((resolve) => {
      const p = spawn(program, ["arg 1", "two"], { env, cwd, stdio: ["inherit", "ignore", "inherit"] });
      p.on("error", (err) => {
        show(`${label} spawn`, err);
        resolve();
      });
      p.on("exit", (code) => {
        console.log(label, "spawn started", code);
        resolve();
      });
    });
  } catch (err) {
    show(`${label} spawn`, err);
  }
  try {
    await new Promise((resolve) => {
      execFile(program, ["arg 1", "two"], { env, cwd }, (err) => {
        if (err && typeof err.code !== "number") {
          show(`${label} exec`, err);
        } else {
          console.log(label, "exec ran", err ? err.code : 0);
        }
        resolve();
      });
    });
  } catch (err) {
    show(`${label} exec`, err);
  }
  try {
    execFileSync(program, ["arg 1", "two"], { env, cwd, stdio: "pipe" });
    console.log(label, "execSync ran", 0);
  } catch (err) {
    show(`${label} execSync`, err);
  }
}

async function main() {
  const dir = `${tmpdir()}\\velt-golden-child-spawn-errors-win-${process.pid}`;
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(`${dir}\\plain.txt`, "not a program\r\n");
  fs.writeFileSync(`${dir}\\fake.exe`, "not a program\r\n");
  fs.writeFileSync(`${dir}\\x.bat`, "@echo ran %*\r\n");
  fs.mkdirSync(`${dir}\\subdir`);
  // A PATH of only a missing directory, so the lookup fails the same way on every machine.
  await attempt("missing", "velt-no-such-program", { PATH: "velt-no-such-dir" }, dir);
  await attempt("not an exe", ".\\fake.exe", {}, dir);
  await attempt("not an exe in PATH", "fake", { PATH: dir }, dir);
  await attempt("text file", ".\\plain.txt", {}, dir);
  await attempt("batch file", ".\\x.bat", {}, dir);
  await attempt("directory", ".\\subdir", {}, dir);

  fs.rmSync(dir, { recursive: true });
}

main();
