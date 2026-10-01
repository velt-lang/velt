# velt:fs_stream

`import { openRead, openWrite } from "velt:fs_stream"`. Reads and writes files in chunks, for
files too large to hold in memory or written incrementally. `FileReader` and `FileWriter` are
Copy handles like `TcpStream`: async methods take `this` by copy, and `close()` releases the
handle exactly once. Failures throw `IoError`.

- `openRead(path): Promise<FileReader>`.
- `FileReader`:
  - `read(max = 0): Promise<u8[]>`: empty at end of file
  - `readString(max = 0): Promise<string>`: `""` at end of file
  - `readLine(): Promise<string | null>`: the line without `\n` / `\r\n`, or null at end of
    file; don't mix it with `readString`
  - `close()`
- `openWrite(path, { append? } = {}): Promise<FileWriter>`: creates or truncates the file,
  unless `append` is set.
- `FileWriter` (64 KiB buffer): `write(data: string)`, `writeBytes(data: u8[])`, `flush()`,
  and `close(): Promise<void>`, which flushes, releases the handle and throws if the final
  flush fails.

```ts
import { openRead, openWrite } from "velt:fs_stream";
import { removeSync } from "velt:fs";
import { tmpdir } from "velt:os";

async function main() {
  const path = `${tmpdir()}/velt-doc-stream.log`;
  const w = await openWrite(path);
  for (let i = 1; i <= 3; i++) {
    await w.write(`line ${i}\n`);
  }
  await w.close();
  const r = await openRead(path);
  let n = 0;
  while (true) {
    const line = await r.readLine();
    if (line == null) {
      break;
    }
    n++;
    console.log(n, line);
  }
  r.close();
  removeSync(path);
}
```

Notes: an unclosed handle leaks until the process exits, and closing through two copies is a
double free. Always `await w.close()` so that write errors are reported.
