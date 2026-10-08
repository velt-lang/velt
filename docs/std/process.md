# velt:process

Write to standard output and read environment variables on the builtin `process`, with no
import, as in Node:

- `process.stdout.write(s)`, `process.stderr.write(s)`: write a string without a newline, ordered
  with `console.log` / `console.error`. They return `true`, like Node.
- `process.env.NAME`, `process.env[name]`: the variable as `string | null`. An unset variable is
  `null` where Node has `undefined`, so `process.env.NAME ?? "default"` reads the same, but
  `process.env.NOPE !== null` is `false` in Velt and `true` in Node. Each read asks the
  operating system again, so a check doesn't narrow a second read: read the variable into a
  `const` and test that. On Windows names ignore case, as in Node.
- `process.argv`: Node's layout, `[runtime, script, ...args]`, so `process.argv.slice(2)` is the
  arguments. The runtime is the running executable, with symbolic links resolved (`velt` under
  `velt dev`). The script is the source file under `velt run` / `velt dev`, the test file under
  `velt test`, and the executable again for a built program (as for a Node single-executable
  application). Under WASI the runtime and the script are the
  module's path; in the browser `process.argv` is `["", ""]`. Each read returns a new array, so
  changing it in place (`process.argv.push(x)`, `process.argv[2] = s`) is an error: copy it
  first (`const argv = process.argv`).
- `process.exit(code: i32)`, `process.memoryUsage()` (below).

`process` is a global, and `velt:process` exports it too: `import { process } from
"velt:process"` names the same builtin, for code that imports it explicitly as Node code does
(`import process from "node:process"`; Velt has named exports only, so the import takes braces).
The builtin is known by its name, so it can't be imported under another one
(`import { process as p }` is an error).

`import { args, cwd } from "velt:process"` for the rest: command-line arguments, listing and
changing variables, the working directory and byte writes.

- `args()`: the arguments only (`process.argv.slice(2)`). `argv()`: every argument, starting with
  the program path (C's `argv`).
- `envAll(): Record<string, string>`: every variable, as a snapshot (Node's `process.env` read as
  an object, so `Object.keys(envAll())` and `Object.entries(envAll())` work as in Node). Names
  are in the order the operating system keeps them, which is Node's order too. On Windows names
  keep their case and the record's lookups are case-sensitive, unlike `process.env.NAME`; the
  per-drive `=C:` entries are left out, as in Node. In the browser the record is empty.
- `setEnv(name, value)`, `removeEnv(name)`. Set variables at startup: writes are not
  synchronized with concurrent reads.
- `cwd()`, `chdir(path)`: both throw `IoError`. `exit(code: i32)`.
- `stdout.write(bytes: u8[])`: raw bytes, without a copy for large arrays.
- `stdout.isTTY`, `stderr.isTTY`, `stdin.isTTY`: whether the stream is a terminal, as Node's
  `process.stdout.isTTY` (`false` when output goes to a file or a pipe, so a CLI can leave out
  colours). `isatty(fd: i32)`: the same for a file descriptor, as Node's `tty.isatty(fd)`; on
  Windows only 0, 1 and 2 can be terminals. The answer for a standard stream is computed once,
  so asking on every write costs nothing. Where Velt differs from Node:
  - `isTTY` is `false` for a stream that isn't a terminal, where Node's is `undefined`.
  - `isatty` is in `velt:process`; Node has it in `node:tty`.
  - On Windows, a mintty or MSYS terminal (Git Bash) counts as a terminal; in Node it doesn't.
  - The builtin `process.stdout` has only `write` so far (`process.stdout.isTTY` is #729):
    import `stdout` for `isTTY`.

```ts
import { stdout } from "velt:process";

function main() {
  const color = stdout.isTTY && process.env.NO_COLOR == null;
  console.log(color ? "\u001b[32mok\u001b[0m" : "ok"); // ok, when piped
}
```

```ts
import { args, envAll, setEnv, cwd } from "velt:process";

function main(): i32 {
  setEnv("GREETING", "hej");
  const greeting = process.env.GREETING ?? "hello";
  process.stdout.write(`${greeting}, ${args().length} arguments
`);
  console.log(process.env.NO_SUCH_VAR, cwd().length > 0);
  console.log(Object.keys(envAll()).includes("GREETING"));
  return 0;
}
```

## Memory usage

`process.memoryUsage()` is built in (no import), with Node's field names. It returns a
`MemoryUsage`, `{ rss: i64; heapUsed: i64 }`, in bytes:

- `rss`: the resident set size the OS reports for the process (Linux, macOS and Windows).
- `heapUsed`: the memory the allocator has committed for the heap: what it took from the OS,
  including freed blocks it keeps for reuse, so it does not drop as soon as objects are freed.
  The runtime doesn't count live bytes, which would slow down every allocation. On Windows it is
  the process's private committed memory.
- On WebAssembly both are the size of the module's linear memory, which only grows.

```ts
const m = process.memoryUsage();
const mib = (bytes: i64): string => ((bytes as f64) / 1048576).toFixed(1);
console.log(`rss ${mib(m.rss)} MiB, heap ${mib(m.heapUsed)} MiB`);
```
