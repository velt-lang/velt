// Runs a wasm32-wasip1 module (argv[2]) under node:wasi with a `random_get` that fails (EIO), so
// the seed of Math.random() can't be read: the program must stop with a panic, not run on with
// a seed of zeros. Prints the exit code; the panic message goes to stderr as usual.
import { readFile } from "node:fs/promises";
import { WASI } from "node:wasi";

const wasi = new WASI({ version: "preview1", returnOnExit: true });
const imports = wasi.getImportObject();
imports.wasi_snapshot_preview1.random_get = () => 29; // EIO
const module = await WebAssembly.compile(await readFile(process.argv[2]));
const instance = await WebAssembly.instantiate(module, imports);
console.log(`exit ${wasi.start(instance)}`);
