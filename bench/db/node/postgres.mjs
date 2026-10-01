// PostgreSQL workloads with pg (bench/db/README.md) against $BENCH_PG_URL: a per-run table,
// dropped at the end. Sequential workloads use one pg.Client with named (prepared) statements;
// the pool workloads use pg.Pool({ max: 8 }). Same SQL and sizes as the Rust baseline.
import pg from "pg";
import { randomUUID } from "node:crypto";
import { now, scale, report, Row, rowChecksum } from "./common.mjs";

const POOL_SIZE = 8;

const toRow = (r) => new Row(r.id, r.name, r.score);

async function insertTx(c, table, n) {
  const text = `INSERT INTO ${table} (id, name, score) VALUES ($1, $2, $3)`;
  const t0 = now();
  await c.query("BEGIN");
  let changes = 0;
  for (let i = 1; i <= n; i++) {
    const r = await c.query({ name: "insert", text, values: [i, `name-${i}`, i + 0.5] });
    changes += r.rowCount;
  }
  await c.query("COMMIT");
  report("postgres.insert_tx", n, changes, now() - t0);
}

function pointQuery(table, i, rows) {
  const text = `SELECT id, name, score FROM ${table} WHERE id = $1`;
  return { name: "point", text, values: [((i * 7919) % rows) + 1] };
}

async function pointSelect(c, table, n, rows) {
  const t0 = now();
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const r = await c.query(pointQuery(table, i, rows));
    if (r.rows.length > 0) sum += rowChecksum(toRow(r.rows[0]));
  }
  report("postgres.point_select", n, sum, now() - t0);
}

async function rangeSelect(c, table, m, rows) {
  const text = `SELECT id, name, score FROM ${table} WHERE id >= $1 AND id < $2`;
  const t0 = now();
  let sum = 0;
  for (let j = 0; j < m; j++) {
    const lo = ((j * 7919) % (rows - 100)) + 1;
    const r = await c.query({ name: "range", text, values: [lo, lo + 100] });
    for (const raw of r.rows) sum += rowChecksum(toRow(raw));
  }
  report("postgres.range_select", m, sum, now() - t0);
}

// One task's share of a pool workload: queries t, t + tasks, t + 2 * tasks, ...
async function poolWorker(pool, table, t, tasks, n, rows) {
  let sum = 0;
  for (let i = t; i < n; i += tasks) {
    const r = await pool.query(pointQuery(table, i, rows));
    if (r.rows.length > 0) sum += rowChecksum(toRow(r.rows[0]));
  }
  return sum;
}

async function poolSelect(pool, table, tasks, n, rows) {
  const t0 = now();
  const workers = [];
  for (let t = 0; t < tasks; t++) workers.push(poolWorker(pool, table, t, tasks, n, rows));
  let sum = 0;
  for (const s of await Promise.all(workers)) sum += s;
  report(`postgres.pool_select_${tasks}`, n, sum, now() - t0);
}

const url = process.env.BENCH_PG_URL;
const table = `bench_db_node_${randomUUID().replaceAll("-", "")}`;
const c = new pg.Client({ connectionString: url });
await c.connect();
await c.query(
  `CREATE TABLE ${table} (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score DOUBLE PRECISION NOT NULL)`,
);
try {
  const rows = 20000 / scale;
  await insertTx(c, table, rows);
  await pointSelect(c, table, 20000 / scale, rows);
  await rangeSelect(c, table, 5000 / scale, rows);
  const pool = new pg.Pool({ connectionString: url, max: POOL_SIZE });
  // Open every pooled connection before timing (the Rust pool is warmed the same way).
  const warm = await Promise.all(Array.from({ length: POOL_SIZE }, () => pool.connect()));
  warm.forEach((client) => client.release());
  await poolSelect(pool, table, 8, 100000 / scale, rows);
  await poolSelect(pool, table, 64, 100000 / scale, rows);
  await pool.end();
} finally {
  await c.query(`DROP TABLE ${table}`);
  await c.end();
}
