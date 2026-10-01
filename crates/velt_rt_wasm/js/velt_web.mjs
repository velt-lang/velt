// JS glue for Velt programs built with `--target wasm32-unknown-unknown`.
//
// The module imports its host services from `velt` (see crates/velt_rt_wasm/src/platform.rs):
//   write(stream, ptr, len)  bytes for stdout (1) / stderr (2)
//   exit(code)               stop the program (thrown as VeltExit, caught by runVelt)
//   now_ms()                 performance.now()
//   date_ms()                Date.now()
// and exports `velt_start()` (runs `main`, returns the exit code) and `memory`.
//
// Browser:  import { runVelt } from "./velt_web.mjs";
//           const code = await runVelt(fetch("hello.wasm"), { write: (s, bytes) => ... });
// Node:     node velt_web.mjs hello.wasm [args...]   (what `velt run --target
//           wasm32-unknown-unknown` does)

/** Thrown by the `exit` import to unwind out of the module. */
export class VeltExit extends Error {
  constructor(code) {
    super(`exit ${code}`);
    this.code = code;
  }
}

/**
 * Instantiate and run a Velt program.
 * @param {BufferSource | Response | Promise<Response>} source the .wasm bytes or a fetch()
 * @param {{ write?: (stream: number, bytes: Uint8Array) => void }} host output sink
 * @returns {Promise<number>} the exit code (101 for a panic, 134 for a WebAssembly trap)
 */
export async function runVelt(source, host = {}) {
  const write = host.write ?? defaultWrite();
  let memory = null;
  const imports = {
    velt: {
      write(stream, ptr, len) {
        write(stream, new Uint8Array(memory.buffer, ptr, len).slice());
      },
      exit(code) {
        throw new VeltExit(code);
      },
      now_ms: () => performance.now(),
      date_ms: () => Date.now(),
    },
  };
  const { instance } = await instantiate(await source, imports);
  memory = instance.exports.memory;
  try {
    return instance.exports.velt_start();
  } catch (e) {
    if (e instanceof VeltExit) return e.code;
    if (e instanceof WebAssembly.RuntimeError) {
      write(2, new TextEncoder().encode(`wasm trap: ${e.message}\n`));
      return 134;
    }
    throw e;
  }
}

async function instantiate(source, imports) {
  if (typeof Response !== "undefined" && source instanceof Response) {
    return WebAssembly.instantiate(await source.arrayBuffer(), imports);
  }
  return WebAssembly.instantiate(source, imports);
}

/** console-based output for browsers: decodes UTF-8 per stream and logs whole lines. */
function defaultWrite() {
  const decoders = { 1: new TextDecoder(), 2: new TextDecoder() };
  const pending = { 1: "", 2: "" };
  return (stream, bytes) => {
    const text = pending[stream] + decoders[stream].decode(bytes, { stream: true });
    const lines = text.split("\n");
    pending[stream] = lines.pop();
    for (const line of lines) (stream === 2 ? console.error : console.log)(line);
  };
}

async function main() {
  const [{ readFile }, { writeSync }] = await Promise.all([
    import("node:fs/promises"),
    import("node:fs"),
  ]);
  const [file] = process.argv.slice(2);
  if (!file) {
    process.stderr.write("usage: node velt_web.mjs <program.wasm> [args...]\n");
    process.exit(2);
  }
  const code = await runVelt(readFile(file), {
    write: (stream, bytes) => writeSync(stream, bytes),
  });
  process.exit(code);
}

if (
  typeof process !== "undefined" &&
  process.argv?.[1] &&
  import.meta.url === (await import("node:url")).pathToFileURL(process.argv[1]).href
) {
  await main();
}
