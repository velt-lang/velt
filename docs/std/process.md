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
