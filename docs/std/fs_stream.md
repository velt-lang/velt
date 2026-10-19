# velt:fs_stream

`import { openRead, openWrite } from "velt:fs_stream"`. Reads and writes files in chunks, for
files too large to hold in memory or written incrementally. `FileReader` and `FileWriter` are
handles like `TcpStream`: you can pass them to tasks and async methods, and `close()` releases
the handle (closing again does nothing). Failures throw `IoError`.

- `openRead(path): Promise<FileReader>`.
- `FileReader`:
  - `read(max = 0): Promise<u8[]>`: empty at end of file
  - `readString(max = 0): Promise<string>`: `""` at end of file
  - `readLine(): Promise<string | null>`: the line without `\n` / `\r\n`, or null at end of
    file; don't mix it with `readString`
  - `lines(): AsyncGenerator<string, IoError>`: the remaining lines, as `readLine` reads them,
    for `for await (const line of r.lines())`. Leaving the loop early keeps the reader open at
    the next line (like Node's `filehandle.readLines()`, but the file is not closed for you)
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
  for await (const line of r.lines()) {
    n++;
    console.log(n, line);
  }
  r.close();
  removeSync(path);
}
```

Notes: an unclosed handle leaks until the process exits. Once one copy of a handle is closed,
the others throw `IoError` `EBADF` (`handle is closed`) and closing again does nothing. Always
`await w.close()` so that write errors are reported.
