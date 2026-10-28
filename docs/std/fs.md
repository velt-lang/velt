# velt:fs

`import { readFile, writeFile, … } from "velt:fs"`. File system access. Every operation has an
async form, which runs on the runtime's blocking pool, and a `*Sync` form. Failures throw
`IoError`.

- `readFile(path): Promise<string>`: UTF-8 text (`EILSEQ` if the file isn't valid UTF-8).
  `readFileBytes(path): Promise<u8[]>`.
- `writeFile(path, data: string)`: creates or truncates. `appendFile(path, data)`.
- `readDir(path): Promise<string[]>`: entry names, sorted.
- `readDirEntries(path): Promise<Dirent[]>`: the entries with their types, sorted by name; Node's
  `readdir(path, { withFileTypes: true })` (a function of its
  own). A `Dirent` has `name` and `isFile()`,
  `isDirectory()`, `isSymbolicLink()`. The type comes with the listing (`d_type` on Linux and
  macOS, the find data on Windows), so a directory walk needs no `stat` per entry; only a file
  system that reports no type costs one. A symlink is `isSymbolicLink()` and nothing else, as
  in Node: `stat` it to see what it points to.
- `stat(path): Promise<Stats>` follows symlinks; `lstat(path)` describes a symlink itself. Node's
  `Stats` shape: `size: u64`, `mtimeMs: f64`, `isFile()`, `isDirectory()`, `isSymbolicLink()`
  (only ever `true` from `lstat`).
- `readlink(path): Promise<string>`: a symlink's target as stored in it (`EINVAL` if `path` is
  not a symlink). `symlink(target, path)`: creates a symlink at `path`; a relative `target` is
  relative to the link's directory. On Windows the link is a directory link when `target` is a
  directory, and a relative `target`'s `/` separators are stored as `\`, both as in Node;
  creating links needs Developer Mode or administrator rights (`EACCES` otherwise).
- `mkdir(path, { recursive? })`, `remove(path, { recursive? })` (a symlink is removed, never what
  it points to), `rename(from, to)`, `copyFile(from, to)`.
- `exists(path): Promise<bool>`: never throws.
- Sync variants: `readFileSync readFileBytesSync writeFileSync appendFileSync readDirSync
  readDirEntriesSync statSync lstatSync readlinkSync symlinkSync mkdirSync removeSync renameSync
  copyFileSync existsSync`.

```ts
import { writeFile, readFile, mkdir, readDir, stat, remove, existsSync } from "velt:fs";
import { tmpdir } from "velt:os";

async function main() {
  const dir = `${tmpdir()}/velt-doc-fs`;
  await mkdir(dir, { recursive: true });
  await writeFile(`${dir}/hello.txt`, "hi\n");
  console.log(await readFile(`${dir}/hello.txt`), (await stat(`${dir}/hello.txt`)).size);
  console.log(await readDir(dir));
  try {
    await readFile(`${dir}/missing.txt`);
  } catch (e) {
    console.log(e.code); // ENOENT
  }
  await remove(dir, { recursive: true });
  console.log(existsSync(dir)); // false
}
```

A directory walk that skips symlinks, as `find` and ripgrep do by default:

```ts
import { readDirEntriesSync } from "velt:fs";

function walk(dir: string, out: string[]) {
  for (const e of readDirEntriesSync(dir)) {
    const path = `${dir}/${e.name}`;
    if (e.isDirectory()) {
      walk(path, out);
    } else if (e.isFile()) {
      out.push(path);
    }
  }
}

function main() {
  const files: string[] = [];
  walk(".", files);
  console.log(files.length > 0); // true
}
```

## File descriptors

Node's file-descriptor API, written in Velt over [system functions](../reference/system-functions.md):
each call is one call of the C library, with no runtime in between. It is the module
`velt:fs/fd` (`import { openSync, readSync } from "velt:fs/fd"`), not `velt:fs` itself, so a
program that only reads and writes whole files does not compile it. On macOS and Linux; for
Windows and WebAssembly, a call of one of them is a compile error at that call, which names the
system function the target lacks (planned: Windows over its kernel32 functions, and `fs.promises`'
`FileHandle`).

- `openSync(path, flags = "r", mode = 0o666): number`: a file descriptor. `flags` is one of
  Node's strings (`"r"`, `"r+"`, `"rs"`, `"rs+"`, `"w"`, `"wx"`, `"w+"`, `"wx+"`, `"a"`, `"ax"`,
  `"as"`, `"a+"`, `"ax+"`, `"as+"`) or the system's `O_*` bits as a number; the descriptor is
  closed on `exec`, as in Node.
- `readSync(fd, buffer: u8[], offset = 0, length?, position?): number`: reads up to `length`
  bytes (default: the rest of the buffer) into `buffer` from `offset` on, at `position` in the
  file (`pread`), or at the file's own position, which advances, when `position` is `null` or
  -1. Returns the bytes read, 0 at the end of the file.
- `writeSync(fd, buffer: u8[] | string, offset?, length?, position?): number`: writes the bytes
  (a string as UTF-8; its third argument is then the position), at `position` or at the file's
  own position (at the end for a file opened to append). Returns the bytes written.
- Arguments are coerced as Node coerces them: a read's `length` is truncated to a 32-bit integer
  (`null` reads 0), a write's is used as given, a read's `position` must be an integer from -1
  (a `RangeError` otherwise) while a write's invalid `position` means the file's own. The
  arguments are positional: Node's `(fd, buffer, options)` form is not supported, so
  `readSync(fd, buf, 4)` reads from offset 4 to the end where Node throws a `TypeError`.
- `fsyncSync(fd)`, `fdatasyncSync(fd)`: flush the file to the disk (on macOS with
  `F_FULLFSYNC`, falling back as Node does). `ftruncateSync(fd, len = 0)`. `closeSync(fd)`.
- `fstatSync(fd): Stats`: the `Stats` of `stat`, from `fstat`. `unlinkSync(path)` removes a file.

Errors are Node's, message for message: an `IoError` with the errno's name as `code` (`ENOENT:
no such file or directory, open 'a.txt'`, `EBADF: bad file descriptor, read`), a `RangeError`
for an offset, length, position or descriptor out of range, and a `TypeError` for unknown
flags. `EINTR` is retried.

```ts
import { openSync, readSync, writeSync, fsyncSync, fstatSync, closeSync, unlinkSync } from "velt:fs/fd";
import { tmpdir } from "velt:os";

function main() {
  const path = `${tmpdir()}/velt-doc-fd.bin`;
  const fd = openSync(path, "w+");
  const page: u8[] = Buffer.alloc(4096);
  page.write("page one");
  writeSync(fd, page, 0, 4096, 0);
  writeSync(fd, page, 0, 4096, 8192); // a hole of zeros in between
  fsyncSync(fd);
  const back: u8[] = Buffer.alloc(8);
  console.log(readSync(fd, back, 0, 8, 8192), back.utf8Slice(0, 8)); // 8 page one
  console.log(fstatSync(fd).size); // 12288
  closeSync(fd);
  unlinkSync(path);
}
```

A call is the system call plus Node's argument checks: a `pread` declared directly costs the
same instructions as Rust's `FileExt::read_at`, and `readSync`'s checks add about 22 per call
(`bench/fs/pread`, bench/RESULTS.md).
