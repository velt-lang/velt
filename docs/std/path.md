# velt:path

`import { join, dirname } from "velt:path"`. POSIX path manipulation (Node's `path.posix`) as pure
string code. The separator is always `/` on every platform.

- `join(a, b)`, `joinAll(parts)`, `normalize(p)`, `dirname(p)`, `basename(p, suffix = "")`,
  `extname(p)`, `isAbsolute(p)`, `sep`.

```ts
import { join, joinAll, dirname, basename, extname, normalize } from "velt:path";

function main() {
  console.log(join("/srv/app", "../data/x.json"), joinAll(["a", "", "b", "c/"]));
  console.log(dirname("/a/b/c.txt"), basename("/a/b/c.txt", ".txt"), extname("archive.tar.gz"));
  console.log(normalize("./a//b/../c")); // a/c
}
```
