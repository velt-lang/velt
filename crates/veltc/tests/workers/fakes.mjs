// In-memory stand-ins for a Worker's bindings, for the tests that drive Workers under node: R2 (a
// Map), D1 (node:sqlite) and KV (a Map). They follow the Workers APIs as far as the programs
// under test use them; workerd's own are checked by hand
// (docs/internals/design/cloudflare-workers.md "As built").
let DatabaseSync;
try {
  ({ DatabaseSync } = await import("node:sqlite"));
} catch {
  // Too old a node: the scenarios that need D1 are skipped.
}
/** Why D1 can't be faked here, if it can't. */
export const d1Missing = DatabaseSync === undefined ? "node:sqlite (node 22.5 or newer)" : undefined;

export class Bucket {
  objects = new Map();
  #meta(key, o) {
    return {
      key,
      size: o.data.length,
      etag: o.etag,
      httpEtag: `"${o.etag}"`,
      uploaded: new Date(1700000000000),
      httpMetadata: { contentType: o.type },
      arrayBuffer: async () => o.data.slice().buffer,
    };
  }
  async get(key) {
    const o = this.objects.get(key);
    return o === undefined ? null : this.#meta(key, o);
  }
  async head(key) {
    return this.get(key);
  }
  async put(key, value, options = {}) {
    if (options.onlyIf?.get?.("if-none-match") === "*" && this.objects.has(key)) return null;
    const data = typeof value === "string" ? new TextEncoder().encode(value) : value;
    const o = { data, etag: `e${data.length}`, type: options.httpMetadata?.contentType };
    this.objects.set(key, o);
    return this.#meta(key, o);
  }
  async delete(key) {
    this.objects.delete(key);
  }
  async list({ prefix = "" } = {}) {
    const objects = [...this.objects.entries()]
      .filter(([k]) => k.startsWith(prefix))
      .map(([k, o]) => this.#meta(k, o));
    return { objects, truncated: false };
  }
}

export class Database {
  #db = new DatabaseSync(":memory:");
  async exec(sql) {
    this.#db.exec(sql);
  }
  prepare(sql) {
    const db = this.#db;
    const statement = (values) => ({
      bind: (...v) => statement(v),
      first: async () => db.prepare(sql).get(...values) ?? null,
      all: async () => ({ results: db.prepare(sql).all(...values) }),
      run: async () => {
        const r = db.prepare(sql).run(...values);
        return { meta: { changes: Number(r.changes), last_row_id: Number(r.lastInsertRowid) } };
      },
      go: () => db.prepare(sql).run(...values),
    });
    return statement([]);
  }
  async batch(statements) {
    this.#db.exec("BEGIN");
    for (const s of statements) s.go();
    this.#db.exec("COMMIT");
  }
}

export class Namespace {
  values = new Map();
  async get(key) {
    return this.values.get(key) ?? null;
  }
  async put(key, value) {
    this.values.set(key, value);
  }
  async delete(key) {
    this.values.delete(key);
  }
  async list({ prefix = "" } = {}) {
    return { keys: [...this.values.keys()].filter((k) => k.startsWith(prefix)).map((name) => ({ name })) };
  }
}
