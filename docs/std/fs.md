# velt:fs

`import { readFile, writeFile, … } from "velt:fs"`. File system access. Every operation has an
async form, which runs on the runtime's blocking pool, and a `*Sync` form. Failures throw
`IoError`.

- `readFile(path): Promise<string>`: UTF-8 text (`EILSEQ` if the file isn't valid UTF-8).
  `readFileBytes(path): Promise<u8[]>`.
- `writeFile(path, data: string)`: creates or truncates. `appendFile(path, data)`.
- `readDir(path): Promise<string[]>`: entry names, sorted.
- `stat(path): Promise<Stats>`, where `Stats { size: u64; mtimeMs: f64; isFile; isDir }`
  (follows symlinks).
- `mkdir(path, { recursive? })`, `remove(path, { recursive? })`, `rename(from, to)`,
  `copyFile(from, to)`.
- `exists(path): Promise<bool>`: never throws.
- Sync variants: `readFileSync readFileBytesSync writeFileSync appendFileSync readDirSync statSync
  mkdirSync removeSync renameSync copyFileSync existsSync`.

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
