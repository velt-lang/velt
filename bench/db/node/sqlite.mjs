// SQLite workloads with better-sqlite3 (bench/db/README.md): a file database in the temp
// directory with WAL and synchronous = NORMAL. Same SQL and sizes as bench/db/velt/sqlite.vlt.
import Database from "better-sqlite3";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";
import { existsSync, rmSync } from "node:fs";
import { now, scale, report, Row, rowChecksum } from "./common.mjs";

const toRow = (r) => new Row(r.id, r.name, r.score);

function insertTx(db, n) {
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

function pointSelect(db, n, rows) {
  const sel = db.prepare("SELECT id, name, score FROM bench WHERE id = :id");
  const t0 = now();
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const raw = sel.get({ id: ((i * 7919) % rows) + 1 });
    if (raw !== undefined) {
      sum += rowChecksum(toRow(raw));
    }
  }
  report("sqlite.point_select", n, sum, now() - t0);
}

function rangeSelect(db, m, rows) {
  const sel = db.prepare("SELECT id, name, score FROM bench WHERE id >= :lo AND id < :hi");
  const t0 = now();
  let sum = 0;
  for (let j = 0; j < m; j++) {
    const lo = ((j * 7919) % (rows - 100)) + 1;
    for (const raw of sel.all({ lo, hi: lo + 100 })) {
      sum += rowChecksum(toRow(raw));
    }
  }
  report("sqlite.range_select", m, sum, now() - t0);
}

function insertAutocommit(db, n) {
  const ins = db.prepare("INSERT INTO bench_auto (id, name, score) VALUES (:id, :name, :score)");
  const t0 = now();
  let changes = 0;
  for (let i = 1; i <= n; i++) {
    changes += ins.run({ id: i, name: `name-${i}`, score: i + 0.5 }).changes;
  }
  report("sqlite.insert_autocommit", n, changes, now() - t0);
}

const path = `${tmpdir()}/node-bench-db-${randomUUID()}.db`;
const db = new Database(path);
db.pragma("journal_mode = WAL");
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
