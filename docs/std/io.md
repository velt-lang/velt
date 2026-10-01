# velt:io

`import { IoError } from "velt:io"`. Defines the error type that the I/O modules share.

- `class IoError extends Error { code: string }`. The codes are `ENOENT EACCES EEXIST EINVAL
  EILSEQ ETIMEDOUT ECONNREFUSED ECONNRESET EADDRINUSE EPIPE EOF ENOTSUP ENOTDIR ENOTEMPTY EISDIR
  UNKNOWN`.
- `IoResult<T>`, `IoStatus`, `ioError`, `unwrapIo` and `checkIo` are exported only for other std
  modules; don't use them in programs.

```ts
import { IoError } from "velt:io";
import { readFileSync } from "velt:fs";

function main() {
  try {
    readFileSync("/definitely/not/here");
  } catch (e) {
    console.log(e instanceof IoError, e.code); // true ENOENT
  }
}
```
