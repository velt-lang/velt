// Runs one compiled program (a wasm32-unknown-unknown module) off the page's main thread, so a
// long-running program never freezes the page and "Stop" can terminate it.
import { runVelt } from "./velt_web.mjs";

onmessage = async (event) => {
  const { module } = event.data;
  try {
    const code = await runVelt(module, {
      write: (stream, bytes) => postMessage({ kind: "output", stream, bytes }, [bytes.buffer]),
    });
    postMessage({ kind: "exit", code });
  } catch (e) {
    postMessage({ kind: "failed", message: String(e) });
  }
};
