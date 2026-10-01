//! SQLite workloads with rusqlite (bench/db/README.md): a file database in the temp directory
//! with WAL and synchronous = NORMAL. Same SQL and sizes as bench/db/velt/sqlite.vlt.

use db_bench::{report, scale, unique_suffix, Row};
use rusqlite::{named_params, Connection, OptionalExtension, Statement};
use std::time::Instant;

fn to_row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        name: r.get(1)?,
        score: r.get(2)?,
    })
}

fn insert(ins: &mut Statement, n: i64) -> rusqlite::Result<i64> {
    let mut changes = 0;
    for i in 1..=n {
        let name = format!("name-{i}");
        let score = i as f64 + 0.5;
        changes += ins.execute(named_params! { ":id": i, ":name": name, ":score": score })? as i64;
    }
    Ok(changes)
}

fn insert_tx(conn: &Connection, n: i64) -> rusqlite::Result<()> {
    let mut ins =
        conn.prepare("INSERT INTO bench (id, name, score) VALUES (:id, :name, :score)")?;
    let t0 = Instant::now();
    conn.execute_batch("BEGIN")?;
    let changes = insert(&mut ins, n)?;
    conn.execute_batch("COMMIT")?;
    report("sqlite.insert_tx", n, changes, t0);
    Ok(())
}

fn point_select(conn: &Connection, n: i64, rows: i64) -> rusqlite::Result<()> {
    let mut sel = conn.prepare("SELECT id, name, score FROM bench WHERE id = :id")?;
    let t0 = Instant::now();
    let mut sum = 0;
    for i in 0..n {
        let row = sel
            .query_row(named_params! { ":id": (i * 7919) % rows + 1 }, to_row)
            .optional()?;
        if let Some(r) = row {
            sum += r.checksum();
        }
    }
    report("sqlite.point_select", n, sum, t0);
    Ok(())
}

fn range_select(conn: &Connection, m: i64, rows: i64) -> rusqlite::Result<()> {
    let mut sel = conn.prepare("SELECT id, name, score FROM bench WHERE id >= :lo AND id < :hi")?;
    let t0 = Instant::now();
    let mut sum = 0;
    for j in 0..m {
        let lo = (j * 7919) % (rows - 100) + 1;
        let found: Vec<Row> = sel
            .query_map(named_params! { ":lo": lo, ":hi": lo + 100 }, to_row)?
            .collect::<rusqlite::Result<_>>()?;
        sum += found.iter().map(Row::checksum).sum::<i64>();
    }
    report("sqlite.range_select", m, sum, t0);
    Ok(())
}

fn insert_autocommit(conn: &Connection, n: i64) -> rusqlite::Result<()> {
    let sql = "INSERT INTO bench_auto (id, name, score) VALUES (:id, :name, :score)";
    let mut ins = conn.prepare(sql)?;
    let t0 = Instant::now();
    let changes = insert(&mut ins, n)?;
    report("sqlite.insert_autocommit", n, changes, t0);
    Ok(())
}

fn main() -> rusqlite::Result<()> {
    let scale = scale();
    let path = std::env::temp_dir().join(format!("rust-bench-db-{}.db", unique_suffix()));
    let conn = Connection::open(&path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch("PRAGMA synchronous = NORMAL")?;
    conn.execute_batch(
        "CREATE TABLE bench (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL NOT NULL);
         CREATE TABLE bench_auto (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL NOT NULL);",
    )?;
    let rows = 1000000 / scale;
    insert_tx(&conn, rows)?;
    point_select(&conn, 1000000 / scale, rows)?;
    range_select(&conn, 50000 / scale, rows)?;
    insert_autocommit(&conn, 100000 / scale)?;
    drop(conn);
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}
