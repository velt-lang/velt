# velt:os

`import { platform } from "velt:os"`. Facts about the machine, a subset of Node's `os` module.

- `platform()`: `"darwin" | "linux" | "win32"`. `arch()`: `"arm64" | "x64"`.
- `availableParallelism()`, `tmpdir()` (no trailing separator), `hostname()` (`""` if unknown),
  `eol()`.

```ts
import { platform, arch, availableParallelism, tmpdir, eol } from "velt:os";

function main() {
  console.log(platform(), arch(), availableParallelism() >= 1, tmpdir(), JSON.stringify(eol()));
}
```
