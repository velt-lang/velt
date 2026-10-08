# velt:fs

`import { readFile, writeFile, … } from "velt:fs"`. File system access. Every operation has an
async form, which runs on the runtime's blocking pool, and a `*Sync` form. Failures throw
`IoError`.

- `readFile(path): Promise<string>`: UTF-8 text (`EILSEQ` if the file isn't valid UTF-8).
  `readFileBytes(path): Promise<u8[]>`.
- `writeFile(path, data: string)`: creates or truncates. `appendFile(path, data)`.
- `readDir(path): Promise<string[]>`: entry names, sorted.
- `readDirEntries(path): Promise<Dirent[]>`: the entries with their types, sorted by name; Node's
  `readdir(path, { withFileTypes: true })` (Velt has no overloads, so it is a function of its
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
