// The Durable Object behind the registry's publish lock (crates/veltc/tests/registry_worker.rs):
// `node lock.mjs <modules> <worker dir>`. Exits 1 and says what failed.

import assert from "node:assert/strict";
import { start } from "./start.mjs";

const [modules, dir] = process.argv.slice(2);
// A lock that never frees would leave this waiting forever.
setTimeout(() => {
  console.error("the lock test timed out: a lock was never handed on");
  process.exit(1);
}, 120_000).unref();

const post = (stub, path, body) => stub.fetch(`https://lock${path}`, { method: "POST", body });
/** The token, or null when another caller holds the lock. */
const acquire = async (stub) => {
  const res = await post(stub, "/acquire");
  const text = await res.text();
  if (res.status === 409) return null;
  assert.equal(res.status, 200);
  return text;
};
const release = async (stub, token) => {
  assert.equal((await post(stub, "/release", token)).status, 204);
};
/** Tries until it holds the lock. */
const acquired = async (stub) => {
  for (;;) {
    const token = await acquire(stub);
    if (token !== null) return token;
    await new Promise((done) => setTimeout(done, 5));
  }
};

// With the default lease, a minute: nothing here runs that long, so nothing expires.
{
  const { mf } = await start(modules, dir);
  const locks = await mf.getDurableObjectNamespace("LOCKS");
  const stub = locks.get(locks.idFromName("one-package"));

  // A held lock is refused until its holder releases it.
  const first = await acquire(stub);
  assert.notEqual(first, null);
  assert.equal(await acquire(stub), null, "a second caller got a held lock");

  // Another package's lock is another object, free meanwhile.
  const other = locks.get(locks.idFromName("another-package"));
  await release(other, await acquire(other));

  await release(stub, first);
  const second = await acquire(stub);
  assert.notEqual(second, null, "the lock stayed held after its release");
  assert.notEqual(second, first);

  // A former holder's release doesn't free the lock of the holder after it.
  await release(stub, first);
  assert.equal(await acquire(stub), null, "an old token freed another holder's lock");
  await release(stub, second);

  // Many at once hold it one at a time. Each holds it over a real round trip, so another holder
  // would be seen.
  const crowded = locks.get(locks.idFromName("a-crowded-package"));
  let holders = 0;
  let most = 0;
  const tokens = await Promise.all(
    Array.from({ length: 8 }, async () => {
      const token = await acquired(crowded);
      holders += 1;
      most = Math.max(most, holders);
      await post(crowded, "/other");
      holders -= 1;
      await release(crowded, token);
      return token;
    }),
  );
  assert.equal(most, 1, "several callers held one lock");
  assert.equal(new Set(tokens).size, 8);

  // Unknown requests are refused.
  assert.equal((await post(stub, "/other")).status, 404);
  await mf.dispose();
}

// A holder that never releases loses the lock when its lease ends.
{
  const { mf } = await start(modules, dir, { LOCK_LEASE_MS: "50" });
  const locks = await mf.getDurableObjectNamespace("LOCKS");
  const stub = locks.get(locks.idFromName("a-dead-holder"));
  const dead = await acquired(stub);
  assert.notEqual(await acquired(stub), dead);
  await mf.dispose();
}

// A setting that isn't a lease is the default: the lock still excludes.
for (const setting of ["", "soon", "0", "-5"]) {
  const { mf } = await start(modules, dir, { LOCK_LEASE_MS: setting });
  const locks = await mf.getDurableObjectNamespace("LOCKS");
  const stub = locks.get(locks.idFromName("a-bad-setting"));
  assert.notEqual(await acquire(stub), null);
  assert.equal(await acquire(stub), null, `LOCK_LEASE_MS "${setting}" stopped the lock excluding`);
  await mf.dispose();
}

console.log("ok");
