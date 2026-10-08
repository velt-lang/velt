# velt:io

`import { IoError } from "velt:io"`. Defines the error type that the I/O modules share.

- `class IoError extends Error { code: string }`. The codes are `ENOENT EACCES EEXIST EINVAL
  EILSEQ ETIMEDOUT ECONNREFUSED ECONNRESET EADDRINUSE EPIPE EOF ENOTSUP ENOTDIR ENOTEMPTY EISDIR
  EBADF UNKNOWN` (`EBADF`: the socket, file stream, child process or WebSocket was closed).
- A failed file-system call (`velt:fs`) has Node's message: the code, the operating system's
  description, the system call and the path, `ENOENT: no such file or directory, open
  'data.txt'` (`rename 'a' -> 'b'` for two paths, and `symlink 'target' -> 'path'`); a failed
  read or write of an opened file names no path (`EISDIR: illegal operation on a directory,
  read`). The system calls are Node's: `open`, `read`, `write`, `scandir`, `stat`, `lstat`,
  `readlink`, `symlink`, `mkdir`, `unlink`, `rmdir`, `rm`, `rename`, `copyfile`. The path is the
  one you passed (Node on Windows prints the absolute path).
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
    console.log(e.message); // ENOENT: no such file or directory, open '/definitely/not/here'
  }
}
```
