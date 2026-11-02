// The entry of a Cloudflare Worker built with `velt build --host host-cloudflare`: Workers' module
// syntax over the runtime glue's host-neutral server core (velt_serve.mjs, beside this file in
// the build's directory, with the program and the host modules it imports).
//
// Workers import a `.wasm` file as its compiled `WebAssembly.Module`. Each request's `env` (the
// Worker's bindings) and `ctx` (its `ExecutionContext`) are the context host modules see
// (`host.env`, `host.ctx`); `ctx.waitUntil` keeps a request's context alive while the program
// still works in it after the answer. Workers give the client's IP in `cf-connecting-ip`.

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
