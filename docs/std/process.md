# velt:process

`import { args, env } from "velt:process"`. Command-line arguments, environment variables, the
working directory and exit.

- `argv()`: every argument, starting with the program path. `args()`: the arguments without the
  program path.
- `env(name): string | null`, `setEnv(name, value)`, `removeEnv(name)`. Set variables at startup:
  writes are not synchronized with concurrent reads.
- `cwd()`, `chdir(path)`: both throw `IoError`. `exit(code: i32)`.

```ts
import { args, env, setEnv, cwd } from "velt:process";

function main(): i32 {
  setEnv("GREETING", "hej");
  console.log(args().length, env("GREETING"), env("NO_SUCH_VAR"), cwd().length > 0);
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
