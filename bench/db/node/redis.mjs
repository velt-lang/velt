// Redis workloads with ioredis (bench/db/README.md) against $BENCH_REDIS_URL, on one client
// (ioredis multiplexes one connection, like std/redis). Same commands and sizes as
// bench/db/velt/redis.vlt.
import Redis from "ioredis";
import { randomUUID } from "node:crypto";
import { now, scale, report } from "./common.mjs";

const BATCH = 100;
const TASKS = 50;

const valueLength = (v) => (v === null ? 0 : v.length);

async function setSeq(c, p, n, keys) {
  const t0 = now();
  let ok = 0;
  for (let i = 0; i < n; i++) {
    const j = i % keys;
    if ((await c.set(`${p}${j}`, `value-${j}`)) === "OK") ok++;
  }
  report("redis.set_seq", n, ok, now() - t0);
}

async function getSeq(c, p, n, keys) {
  const t0 = now();
  let sum = 0;
  for (let i = 0; i < n; i++) {
    sum += valueLength(await c.get(`${p}${(i * 7919) % keys}`));
  }
  report("redis.get_seq", n, sum, now() - t0);
}

async function pipelineSet(c, p, n, keys) {
  const t0 = now();
  let ok = 0;
  for (let b = 0; b < n; b += BATCH) {
    const pl = c.pipeline();
    for (let i = b; i < b + BATCH; i++) {
      const j = i % keys;
      pl.set(`${p}${j}`, `value-${j}`);
    }
    for (const [err, r] of await pl.exec()) {
      if (err === null && r === "OK") ok++;
    }
  }
  report("redis.pipeline_set", n, ok, now() - t0);
}

async function pipelineGet(c, p, n, keys) {
  const t0 = now();
  let sum = 0;
  for (let b = 0; b < n; b += BATCH) {
    const pl = c.pipeline();
    for (let i = b; i < b + BATCH; i++) {
      pl.get(`${p}${(i * 7919) % keys}`);
    }
    for (const [err, r] of await pl.exec()) {
      if (err === null) sum += valueLength(r);
    }
  }
  report("redis.pipeline_get", n, sum, now() - t0);
}

async function getWorker(c, p, t, n, keys) {
  let sum = 0;
  for (let i = t; i < n; i += TASKS) {
    sum += valueLength(await c.get(`${p}${(i * 7919) % keys}`));
  }
  return sum;
}

async function concurrentGet(c, p, n, keys) {
  const t0 = now();
  const tasks = [];
  for (let t = 0; t < TASKS; t++) tasks.push(getWorker(c, p, t, n, keys));
  let sum = 0;
  for (const s of await Promise.all(tasks)) sum += s;
  report("redis.concurrent_get", n, sum, now() - t0);
}

async function cleanup(c, p, keys) {
  for (let b = 0; b < keys; b += 1000) {
    const batch = [];
    for (let j = b; j < b + 1000 && j < keys; j++) batch.push(`${p}${j}`);
    await c.del(batch);
  }
}

const c = new Redis(process.env.BENCH_REDIS_URL ?? "redis://127.0.0.1:6379");
const p = `node-bench-db:${randomUUID()}:`;
const keys = 20000 / scale;
await setSeq(c, p, 40000 / scale, keys);
await getSeq(c, p, 40000 / scale, keys);
await pipelineSet(c, p, 1000000 / scale, keys);
await pipelineGet(c, p, 1000000 / scale, keys);
await concurrentGet(c, p, 400000 / scale, keys);
await cleanup(c, p, keys);
c.disconnect();
