// The requests for bindings.vlt, and its bindings (fakes.mjs), a Durable Object (a counter) and
// the cache.
import { Bucket, Database, Namespace, d1Missing } from "./fakes.mjs";

export const skip = d1Missing;

export const requests = () =>
  ["/r2", "/d1", "/kv", "/do", "/cache", "/missing", "/other"].map(
    (p) => new Request(`https://example.com${p}`),
  );

const counts = new Map();
const objects = {
  idFromName: (name) => name,
  get: (id) => ({
    async fetch(url, init) {
      const n = (counts.get(id) ?? 0) + 1;
      counts.set(id, n);
      return new Response(`${id}=${n} ${init.method} ${new URL(url).pathname}`);
    },
  }),
};

export const env = d1Missing !== undefined
  ? {}
  : { BLOBS: new Bucket(), DB: new Database(), KV: new Namespace(), COUNTER: objects };

const cached = new Set(["https://example.com/cached"]);
export const caches = { default: { delete: async (url) => cached.delete(url) } };
