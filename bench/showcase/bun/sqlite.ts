// The SQLite workloads of bench/db (README.md there) on bun:sqlite, Bun's built-in driver: the
// same SQL, sizes, parameters and checksums as bench/db/node/sqlite.mjs (better-sqlite3, a
// native addon, is Node's driver). A file database in the temp directory with WAL and
// synchronous = NORMAL. Usage: bun sqlite.ts [quick]
import { Database } from "bun:sqlite";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";
import { existsSync, rmSync } from "node:fs";
import { now, scale, report, Row, rowChecksum } from "../../db/node/common.mjs";

type Raw = { id: number; name: string; score: number };
const toRow = (r: Raw) => new Row(r.id, r.name, r.score);

function insertTx(db: Database, n: number) {
  const ins = db.prepare("INSERT INTO bench (id, name, score) VALUES (:id, :name, :score)");
  const t0 = now();
  db.exec("BEGIN");
  let changes = 0;
  for (let i = 1; i <= n; i++) {
    changes += ins.run({ id: i, name: `name-${i}`, score: i + 0.5 }).changes;
  }
  db.exec("COMMIT");
  report("sqlite.insert_tx", n, changes, now() - t0);
}

function pointSelect(db: Database, n: number, rows: number) {
  const sel = db.prepare("SELECT id, name, score FROM bench WHERE id = :id");
  const t0 = now();
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const raw = sel.get({ id: ((i * 7919) % rows) + 1 }) as Raw | null;
    if (raw !== null) {
      sum += rowChecksum(toRow(raw));
    }
  }
  report("sqlite.point_select", n, sum, now() - t0);
}

function rangeSelect(db: Database, m: number, rows: number) {
  const sel = db.prepare("SELECT id, name, score FROM bench WHERE id >= :lo AND id < :hi");
  const t0 = now();
  let sum = 0;
  for (let j = 0; j < m; j++) {
    const lo = ((j * 7919) % (rows - 100)) + 1;
    for (const raw of sel.all({ lo, hi: lo + 100 }) as Raw[]) {
      sum += rowChecksum(toRow(raw));
    }
  }
  report("sqlite.range_select", m, sum, now() - t0);
}

function insertAutocommit(db: Database, n: number) {
  const ins = db.prepare("INSERT INTO bench_auto (id, name, score) VALUES (:id, :name, :score)");
  const t0 = now();
  let changes = 0;
  for (let i = 1; i <= n; i++) {
    changes += ins.run({ id: i, name: `name-${i}`, score: i + 0.5 }).changes;
  }
  report("sqlite.insert_autocommit", n, changes, now() - t0);
}

const path = `${tmpdir()}/bun-bench-db-${randomUUID()}.db`;
// strict: named parameters bind from object keys without their `:` prefix, as in better-sqlite3.
const db = new Database(path, { strict: true });
db.exec("PRAGMA journal_mode = WAL");
db.exec("PRAGMA synchronous = NORMAL");
db.exec("CREATE TABLE bench (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL NOT NULL)");
db.exec("CREATE TABLE bench_auto (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL NOT NULL)");
const rows = 1000000 / scale;
insertTx(db, rows);
pointSelect(db, 1000000 / scale, rows);
rangeSelect(db, 50000 / scale, rows);
insertAutocommit(db, 100000 / scale);
db.close();
for (const suffix of ["", "-wal", "-shm"]) {
  if (existsSync(path + suffix)) rmSync(path + suffix);
}
