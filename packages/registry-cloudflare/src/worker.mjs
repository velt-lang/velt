// The entry of the registry's Worker, `velt build --host registry-cloudflare`: host-cloudflare's
// entry (see its comments), and the class of the Durable Object behind `DurableObjectPublishLock`.

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

/** How long a holder that never releases keeps a lock, unless `LOCK_LEASE_MS` says otherwise. */
const LEASE_MS = 60_000;

/** `LOCK_LEASE_MS` when it is a positive whole number of milliseconds, else the default. */
function leaseMs(value) {
  const ms = Number(value);
  return Number.isSafeInteger(ms) && ms > 0 ? ms : LEASE_MS;
}

/**
 * One package's lock. `POST /acquire` answers at once: 200 with a token when this caller now holds
 * the lock, 409 when another does, and the caller tries again. So nobody waits in here, and a
 * caller that went away never gets the lock with no one left to release it. `POST /release` with
 * the token as its body frees it. A holder that died (a Worker that was killed) loses the lock when
 * its lease ends.
 *
 * The holder is read and set in memory, with no `await` between. Storage has a copy, so an object
 * that restarted still honours a lease; the answer waits for the write.
 */
export class RegistryLock {
  /** The holder, `{ token, until }`, or undefined. */
  #lease;

  constructor(state, env) {
    this.state = state;
    this.leaseMs = leaseMs(env.LOCK_LEASE_MS);
    state.blockConcurrencyWhile(async () => {
      this.#lease = await state.storage.get("lease");
    });
  }

  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (request.method === "POST" && path === "/acquire") {
      const token = await this.#acquire();
      return token === null ? new Response("held", { status: 409 }) : new Response(token);
    }
    if (request.method === "POST" && path === "/release") {
      await this.#release(await request.text());
      return new Response(null, { status: 204 });
    }
    return new Response("not found", { status: 404 });
  }

  async #acquire() {
    const now = Date.now();
    if (this.#lease !== undefined && this.#lease.until > now) return null;
    const token = crypto.randomUUID();
    this.#lease = { token, until: now + this.leaseMs };
    await this.state.storage.put("lease", this.#lease);
    return token;
  }

  async #release(token) {
    // A holder whose lease ended no longer holds it: the next holder keeps the lock.
    if (this.#lease?.token === token) {
      this.#lease = undefined;
      await this.state.storage.delete("lease");
    }
  }
}
