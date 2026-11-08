// The entry of the registry's Worker, `velt build --host registry-cloudflare`: host-cloudflare's
// entry (see its comments).

import module from "./program.wasm";
import { createServer } from "./velt_serve.mjs";
import { modules } from "./velt_modules.mjs";

const server = createServer(module, { modules });

export default {
  fetch(request, env, ctx) {
    return server.serve(request, {
      context: { env, ctx },
      waitUntil: (p) => ctx.waitUntil(p),
      remoteAddr: request.headers.get("cf-connecting-ip") ?? "",
    });
  },
};
