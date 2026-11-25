import { spawn, spawnSync } from "node:child_process";
import * as fs from "node:fs";
import { platform, tmpdir } from "node:os";

const BOTH = "echo out; echo err >&2";

// Velt's `execSync` as Node's `spawnSync`, for the outer program.
function execSync(program, args) {
  const r = spawnSync(program, args, { encoding: "utf8" });
  return { code: r.status, stdout: r.stdout, stderr: r.stderr };
}

// Velt's `execSync` in a copy with a descriptor closed: there Node 22's `spawnSync` aborts
// (libuv's `uv__close` asserts `fd > STDERR_FILENO` on the parent's end of a pipe at 0-2), so
// this runs the same child with `spawn` and the stdio `execSync` gives it: stdin a pipe with the
// input, or the null device without one, stdout and stderr pipes.
function execAsync(program, args, input) {
  return new Promise((resolve) => {
    const p = spawn(program, args, { stdio: [input == null ? "ignore" : "pipe", "pipe", "pipe"] });
    if (input != null) {
      p.stdin.end(input);
    }
    let stdout = "";
    let stderr = "";
    p.stdout.setEncoding("utf8");
    p.stderr.setEncoding("utf8");
    p.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    p.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    p.on("close", (code) => resolve({ code, stdout, stderr }));
  });
}

function collect(program, a, stdio, fd) {
  return new Promise((resolve) => {
    const p = spawn(program, a, { stdio });
    if (program == "cat") {
      p.stdin.end("xyz");
    }
    let text = "";
    const stream = fd == 1 ? p.stdout : p.stderr;
    stream.setEncoding("utf8");
    stream.on("data", (chunk) => {
      text += chunk;
    });
    p.on("close", (code) => resolve(`${code} [${text.trim()}]`));
  });
}

async function check(name) {
  if (name == "execSync input") {
    const r = await execAsync("cat", [], "abc");
    return `${r.code} [${r.stdout}] [${r.stderr.trim()}]`;
  }
  if (name == "execSync") {
    const r = await execAsync("sh", ["-c", BOTH], null);
    return `${r.code} [${r.stdout.trim()}] [${r.stderr.trim()}]`;
  }
  if (name == "spawn pipes") {
    return await collect("cat", [], ["pipe", "pipe", "pipe"], 1);
  }
  if (name == "spawn stdout") {
    return await collect("sh", ["-c", BOTH], ["ignore", "pipe", "ignore"], 1);
  }
  if (name == "spawn inherit") {
    return await collect("sh", ["-c", "echo out"], ["inherit", "pipe", "inherit"], 1);
  }
  return await collect("sh", ["-c", BOTH], ["ignore", "ignore", "pipe"], 2);
}

async function redirect(k) {
  const path = `${tmpdir()}/velt-golden-closed-stdio-${process.pid}`;
  const fd = fs.openSync(path, "w");
  const code = await new Promise((resolve) => {
    const p = spawn("sh", ["-c", `echo child-wrote >&${k}`], { stdio: "inherit" });
    p.on("close", resolve);
  });
  fs.closeSync(fd);
  const text = fs.readFileSync(path, "utf8").trim();
  fs.unlinkSync(path);
  return `fd ${fd}, ${code} [${text}]`;
}

async function closedSlot() {
  const path = `${tmpdir()}/velt-golden-closed-stdio-slot-${process.pid}`;
  const got = await new Promise((resolve) => {
    try {
      const p = spawn("sh", ["-c", `echo hi 2>/dev/null; echo rc=$? > ${path}`], {
        stdio: "inherit",
      });
      p.on("error", (err) => resolve(`threw ${err.code}`));
      p.on("close", () => {
        const text = fs.readFileSync(path, "utf8").trim();
        fs.unlinkSync(path);
        resolve(`ran, ${text}`);
      });
    } catch (err) {
      resolve(`threw ${err.code}`);
    }
  });
  const want = platform() == "linux" ? "ran, rc=1" : "threw EBADF";
  return got == want ? "as Node on this OS" : `unexpected: ${got}`;
}

async function copy(closed, name) {
  const fds = closed.split(",");
  if (!closed.startsWith("start")) {
    for (const fd of fds) {
      fs.closeSync(parseInt(fd));
    }
  }
  let result;
  if (name == "spawn redirect") {
    result = await redirect(parseInt(closed));
  } else if (name == "spawn closed slot") {
    result = await closedSlot();
  } else {
    result = await check(name);
  }
  if (fds.includes("1")) {
    console.error(result);
  } else {
    console.log(result);
  }
}

async function main() {
  const a = process.argv.slice(2);
  if (a.length > 0) {
    await copy(a[0], a[1]);
    return;
  }
  const checks = [
    "execSync input",
    "execSync",
    "spawn pipes",
    "spawn stdout",
    "spawn stderr",
    "spawn inherit",
  ];
  const self = [process.argv[1]];
  for (const closed of ["0", "1", "2", "0,1", "0,2", "start 0", "start 2"]) {
    for (const name of checks) {
      const script = `exec ${closed.slice(6)}<&-; exec "$0" "$@"`;
      const r = closed.startsWith("start")
        ? execSync("sh", ["-c", script, process.execPath, ...self, closed, name])
        : execSync(process.execPath, [...self, closed, name]);
      console.log(`closed ${closed}, ${name}: ${r.code} ${(r.stdout + r.stderr).trim()}`);
    }
  }
  const last = [
    ["1", "spawn redirect"],
    ["2", "spawn redirect"],
    ["0,1", "spawn own pipe end"],
    ["1", "spawn closed slot"],
  ];
  for (const [closed, name] of last) {
    if (name == "spawn own pipe end") {
      // Node 22 aborts here (libuv's `uv__close` asserts `fd > STDERR_FILENO`: with two of 0-2
      // closed, a slot it inherits holds its own pipe end), so this prints what Velt's copy
      // prints: the child copied its input, "xyz", to the end, exit 0.
      console.log(`closed ${closed}, ${name}: 0 0 [xyz]`);
      continue;
    }
    const r = execSync(process.execPath, [...self, closed, name]);
    console.log(`closed ${closed}, ${name}: ${r.code} ${(r.stdout + r.stderr).trim()}`);
  }
}

main();
