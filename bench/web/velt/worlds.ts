// The database side of the TechEmpower routes (bench/web/README.md), shared by the Velt servers:
// velt/app.ts (std/http) and velt-native/server.vlt (velt/http/server). /db, /queries?queries=N,
// /fortunes and /updates?queries=N on std/postgres.

import { escapeHtml } from "velt/text";
import { availableParallelism } from "velt/os";
import { createPool, Client, Pool } from "velt/postgres";
import { randomInt } from "velt/random";

export const DEFAULT_URL = "postgres://benchmarkdbuser:benchmarkdbpass@127.0.0.1:5432/hello_world";
const SELECT_WORLD = "SELECT id, randomnumber AS \"randomNumber\" FROM world WHERE id = $1";
const SELECT_FORTUNES = "SELECT id, message FROM fortune";
// One statement for any N: the sorted ids and new numbers travel as two int arrays.
const UPDATE_WORLDS =
  "UPDATE world SET randomnumber = u.r FROM (SELECT unnest($1::int[]) AS id, unnest($2::int[]) AS r) AS u WHERE world.id = u.id";
// From this many rows on, a request's queries go as one batch (one Sync, like pgx's Batch).
const BATCH_FROM = 5;
export const HTML = "text/html; charset=utf-8";

export class World {
  id: i64;
  randomNumber: i64;

  constructor(id: i64, randomNumber: i64) {
    this.id = id;
    this.randomNumber = randomNumber;
  }
}

export class Fortune {
  id: i64;
  message: string;

  constructor(id: i64, message: string) {
    this.id = id;
    this.message = message;
  }
}

export class Message {
  message: string;

  constructor(message: string) {
    this.message = message;
  }
}

// `n` numbers in 1..10000.
function randomIds(n: i64): i64[] {
  const ids: i64[] = [];
  for (let i: i64 = 0; i < n; i++) {
    ids.push(randomInt(1, 10001));
  }
  return ids;
}

// `?queries=N`, clamped to 1..500; missing or not a number is 1.
export function queryCount(query: string): i64 {
  let raw = "";
  for (const part of query.split("&")) {
    if (part.startsWith("queries=")) {
      raw = part.substring(8);
    }
  }
  const n = parseInt(raw, 10);
  if (isNaN(n) || n < 1.0) {
    return 1;
  }
  if (n > 500.0) {
    return 500;
  }
  return n as i64;
}

// One row; a failed query yields id 0 so the caller can answer 500 (joined promises can't throw).
async function fetchWorld(c: Client, id: i64): Promise<World> {
  try {
    const w: World | null = await c.queryOne(SELECT_WORLD, [id]);
    if (w != null) {
      return w;
    }
  } catch (e) {
    console.log("query failed:", e.message);
  }
  return new World(0, 0);
}

// `n` random rows, queried concurrently (pipelined) on client `c`; empty if a query failed
// (joined promises can't throw).
async function fetchWorlds(c: Client, n: i64): Promise<World[]> {
  const pending: Promise<World>[] = [];
  for (const id of randomIds(n)) {
    pending.push(fetchWorld(c, id));
  }
  const worlds = await Promise.all(pending);
  for (const w of worlds) {
    if (w.id == 0) {
      return [];
    }
  }
  return worlds;
}

// `n` random ids as batch parameter sets (`[[id], ...]`).
function idSets(n: i64): i64[][] {
  const sets: i64[][] = [];
  for (const id of randomIds(n)) {
    sets.push([id]);
  }
  return sets;
}

// The rows of a batch of `SELECT_WORLD`s; throws if one is missing.
function batchWorlds(rows: (World | null)[]): World[] {
  const worlds: World[] = [];
  for (const w of rows) {
    if (w == null) {
      throw new Error("no such world");
    }
    worlds.push(new World(w.id, w.randomNumber));
  }
  return worlds;
}

// `n` random rows on `c`: one query for a single row, a batch from `BATCH_FROM` rows.
async function worldsOn(c: Client, n: i64): Promise<World[]> {
  if (n >= BATCH_FROM) {
    const rows: (World | null)[] = await c.batchQueryOne(SELECT_WORLD, idSets(n));
    return batchWorlds(rows);
  }
  if (n == 1) {
    const w: World | null = await c.queryOne(SELECT_WORLD, [randomInt(1, 10001)]);
    if (w == null) {
      throw new Error("no such world");
    }
    return [w];
  }
  const worlds = await fetchWorlds(c, n);
  if (worlds.length == 0) {
    throw new Error("a world query failed");
  }
  return worlds;
}

// `n` random rows: one pooled query for a single row, a pooled batch from `BATCH_FROM` rows,
// else pipelined on a dedicated connection.
export async function loadWorlds(pool: Pool, n: i64): Promise<World[]> {
  if (n >= BATCH_FROM) {
    const rows: (World | null)[] = await pool.batchQueryOne(SELECT_WORLD, idSets(n));
    return batchWorlds(rows);
  }
  if (n == 1) {
    const w: World | null = await pool.queryOne(SELECT_WORLD, [randomInt(1, 10001)]);
    if (w == null) {
      throw new Error("no such world");
    }
    return [w];
  }
  const c = await pool.connect();
  try {
    const worlds = await worldsOn(c, n);
    c.close();
    return worlds;
  } catch (e) {
    c.close();
    throw e;
  }
}

// Postgres array literal `{a,b,c}`.
function intArray(xs: i64[]): string {
  const parts: string[] = [];
  for (const x of xs) {
    parts.push(`${x}`);
  }
  return `{${parts.join(",")}}`;
}

// Reads `n` rows, gives each a new random number and writes them back in one statement (rows
// sorted by id, so concurrent updates lock in the same order).
export async function updateWorlds(pool: Pool, n: i64): Promise<World[]> {
  const c = await pool.connect();
  try {
    const updated = await updateOn(c, n);
    c.close();
    return updated;
  } catch (e) {
    c.close();
    throw e;
  }
}

// The reads and the write of `/updates` on one connection, as the other implementations do.
async function updateOn(c: Client, n: i64): Promise<World[]> {
  const worlds = await worldsOn(c, n);
  const numbers = randomIds(n);
  const updated: World[] = [];
  for (let i: i64 = 0; i < n; i++) {
    updated.push(new World(worlds[i].id, numbers[i]));
  }
  const sorted = updated.clone();
  sorted.sort((a, b) => a.id - b.id);
  const ids: i64[] = [];
  const values: i64[] = [];
  for (const w of sorted) {
    ids.push(w.id);
    values.push(w.randomNumber);
  }
  await c.execute(UPDATE_WORLDS, [intArray(ids), intArray(values)]);
  return updated;
}

// `render`: the fortunes page of the sorted fortunes, with the doctype; what it throws makes the
// request fail with 500.
export async function fortunesHtml<E>(
  pool: Pool,
  render: (fortunes: Fortune[]) => string throws E,
): Promise<string> {
  const fortunes: Fortune[] = await pool.select(SELECT_FORTUNES);
  fortunes.push(new Fortune(0, "Additional fortune added at request time."));
  fortunes.sort((a, b) => a.message.compareTo(b.message));
  return render(fortunes);
}

const FORTUNES_HEAD =
  "<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table><tr><th>id</th><th>message</th></tr>";

/** The fortunes page as a template literal per row. */
export function fortunesPage(fortunes: Fortune[]): string {
  const parts: string[] = [FORTUNES_HEAD];
  for (const f of fortunes) {
    parts.push(`<tr><td>${f.id}</td><td>${escapeHtml(f.message)}</td></tr>`);
  }
  parts.push("</table></body></html>");
  return parts.join("");
}

/** The database pool: `DATABASE_URL`, `DB_POOL` connections (default 2 × cores). */
export function worldPool(): Pool {
  const raw = process.env.DB_POOL ?? "";
  const max = raw == "" ? availableParallelism() * 2 : parseInt(raw, 10) as i64;
  return createPool({ url: process.env.DATABASE_URL ?? DEFAULT_URL, max: max });
}
