# velt:child_process

`import { exec, execSync, spawn } from "velt:child_process"`. Runs other programs. Programs are
found through `PATH` and arguments are passed as-is. A non-zero exit code is not an error;
failing to start the program throws `IoError` (e.g. `ENOENT`).

- `exec(program, args = [], opts: ExecOptions = {}): Promise<ExecResult>` and `execSync(…)`.
  `ExecOptions { cwd?; env?: Map; clearEnv?; input? }`.
- `execShell(command, opts)` / `execShellSync`: run through `sh -c` or `cmd /C`. Never pass
  untrusted text.
- `ExecResult { code; stdout; stderr; ok }`: `code` is `128 + n` when the process was killed by
  signal `n`.
- `spawn(program, args = [], opts: SpawnOptions = {}): ChildProcess`. `SpawnOptions` has
  `cwd env clearEnv stdin stdout stderr`; the stdio fields take `"pipe"`, `"inherit"` (the
  default) or `"ignore"`.
- `ChildProcess { pid }`: a handle like `TcpStream`, released by `close()`.
  - `write(data)`, `closeStdin()`
  - `readStdout(max = 0)` / `readStderr`: `""` means end of output
  - `readStdoutBytes` / `readStderrBytes`
  - `wait(): Promise<i64>`, `exitCode: i64 | null`
  - `kill(signal = "SIGTERM")`, `close()`
  - Errors from awaited methods propagate to `catch`.

```ts
import { exec, execSync, spawn } from "velt:child_process";

async function main() {
  const r = execSync("echo", ["hello", "world"]);
  console.log(r.ok, r.code, r.stdout.trim());
  const sorted = await exec("sort", [], { input: "b\na\n" });
  console.log(JSON.stringify(sorted.stdout)); // "a\nb\n"
  const child = spawn("cat", [], { stdin: "pipe", stdout: "pipe" });
  await child.write("piped\n");
  await child.closeStdin();
  console.log(JSON.stringify(await child.readStdout()), await child.wait());
  child.close();
}
```

Notes:
- `close()` doesn't stop the program, which is reaped in the background; `kill` it or `wait`
  for it. An unclosed handle leaks until the process exits.
- On Windows every signal terminates the process.
- An unsupported signal name throws `IoError` "EINVAL".
