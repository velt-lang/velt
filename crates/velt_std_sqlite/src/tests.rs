//! Tests of the driver's operations: binding, row encoding, errors and handle lifetimes.

use super::connection::*;
use super::error::{code_name, DbError};
use super::statement::{self, StmtHandle};
use velt_native::handles::Key;

fn text(e: DbError) -> String {
    format!("{}: {}", code_name(e.code), e.message)
}

fn open_memory() -> DbHandle {
    open(":memory:", false, true, 1000).expect("open :memory:")
}

fn exec(db: DbHandle, sql: &str) -> Result<(), String> {
    super::connection::exec(db, sql).map_err(text)
}

fn prepare(db: DbHandle, sql: &str) -> Result<StmtHandle, String> {
    statement::prepare(db, sql).map_err(text)
}

fn run(stmt: StmtHandle, params: &str) -> Result<(i64, i64), String> {
    statement::run(stmt, params.as_bytes())
        .map(|r| (r.changes, r.last_insert_rowid))
        .map_err(text)
}

/// The rows as the JSON text they stand for (`""` for no row with `first_only`).
fn query(stmt: StmtHandle, params: &str, first_only: bool) -> Result<String, String> {
    let rows = statement::query(stmt, params.as_bytes(), first_only, Vec::new()).map_err(text)?;
    if rows.is_empty() {
        return Ok(String::new());
    }
    Ok(String::from_utf8(velt_text::rows::to_json(&rows).unwrap()).unwrap())
}

fn sql_query(db: DbHandle, sql: &str, params: &str) -> Result<String, String> {
    let stmt = prepare(db, sql)?;
    let r = query(stmt, params, false);
    unsafe { statement::free(stmt) };
    r
}

fn close(db: DbHandle) {
    super::connection::close(db).map_err(text).expect("close");
}

const NULL_DB: DbHandle = Key::NULL;
const NULL_STMT: StmtHandle = velt_native::handles::Single::NULL;

#[test]
fn insert_and_select_with_named_and_positional_params() {
    let db = open_memory();
    exec(
        db,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score REAL)",
    )
    .unwrap();
    let ins = prepare(db, "INSERT INTO t (name, score) VALUES (:name, @score)").unwrap();
    assert_eq!(
        run(ins, r#"{"name":"ann","score":1.5,"extra":0}"#),
        Ok((1, 1))
    );
    assert_eq!(run(ins, r#"{"score":2,"name":"bob"}"#), Ok((1, 2)));
    let by_pos = prepare(db, "INSERT INTO t (name, score) VALUES (?, ?)").unwrap();
    assert_eq!(run(by_pos, r#"["cy",null]"#), Ok((1, 3)));
    let sel = prepare(
        db,
        "SELECT id, name, score FROM t WHERE id >= $min ORDER BY id",
    )
    .unwrap();
    assert_eq!(
        query(sel, r#"{"min":2}"#, false).unwrap(),
        r#"[{"id":2,"name":"bob","score":2},{"id":3,"name":"cy","score":null}]"#
    );
    assert_eq!(
        query(sel, r#"{"min":1}"#, true).unwrap(),
        r#"{"id":1,"name":"ann","score":1.5}"#
    );
    assert_eq!(query(sel, r#"{"min":9}"#, true).unwrap(), "");
    assert_eq!(query(sel, r#"{"min":9}"#, false).unwrap(), "[]");
    let one = prepare(db, "SELECT count(*) AS n FROM t WHERE id > ?").unwrap();
    assert_eq!(query(one, "1", true).unwrap(), r#"{"n":2}"#);
    for h in [ins, by_pos, sel, one] {
        unsafe { statement::free(h) };
    }
    close(db);
}

#[test]
fn values_round_trip_with_their_types() {
    let db = open_memory();
    exec(
        db,
        "CREATE TABLE v (i INTEGER, f REAL, t TEXT, b BLOB, n, ok BOOLEAN)",
    )
    .unwrap();
    let params = r#"[9223372036854775807, 0.1, "q\"\né", [0,255], null, true]"#;
    let ins = prepare(db, "INSERT INTO v VALUES (?, ?, ?, ?, ?, ?)").unwrap();
    run(ins, params).unwrap();
    run(ins, r#"[-9007199254740993, 1e300, "", [], 1, false]"#).unwrap();
    assert_eq!(
        sql_query(db, "SELECT * FROM v", "").unwrap(),
        concat!(
            r#"[{"i":9223372036854775807,"f":0.1,"t":"q\"\né","b":[0,255],"n":null,"ok":true},"#,
            // The float as `rows_bin::to_json` writes it (JavaScript's form).
            r#"{"i":-9007199254740993,"f":1e+300,"t":"","b":[],"n":1,"ok":false}]"#
        )
    );
    // Booleans only come back as `true`/`false` from a BOOLEAN column, not from expressions.
    assert_eq!(
        sql_query(
            db,
            "SELECT ok, ok AND 1 AS e, 1.0 / 0 AS inf FROM v LIMIT 1",
            ""
        )
        .unwrap(),
        r#"[{"ok":true,"e":1,"inf":null}]"#
    );
    unsafe { statement::free(ins) };
    close(db);
}

#[test]
fn parameter_errors_are_sqlite_range() {
    let db = open_memory();
    let named = prepare(db, "SELECT :a AS a, :b AS b").unwrap();
    let e = query(named, r#"{"a":1}"#, false).unwrap_err();
    assert_eq!(e, r#"SQLITE_RANGE: missing named parameter "b""#);
    assert!(query(named, "", false)
        .unwrap_err()
        .contains("none were given"));
    assert_eq!(query(named, "[1,2]", false).unwrap(), r#"[{"a":1,"b":2}]"#);
    let pos = prepare(db, "SELECT ? AS x").unwrap();
    assert!(query(pos, r#"{"x":1}"#, false)
        .unwrap_err()
        .contains("positional"));
    assert!(query(pos, "[1,2]", false)
        .unwrap_err()
        .contains("but 2 value(s)"));
    assert!(query(pos, r#"[{"no":1}]"#, false)
        .unwrap_err()
        .contains("nested objects"));
    unsafe { statement::free(named) };
    unsafe { statement::free(pos) };
    close(db);
}

#[test]
fn sqlite_errors_carry_codes_and_are_counted() {
    let db = open_memory();
    let count = || error_count(db);
    exec(db, "CREATE TABLE u (email TEXT UNIQUE NOT NULL)").unwrap();
    assert_eq!(count(), 0);
    let ins = prepare(db, "INSERT INTO u VALUES (?)").unwrap();
    run(ins, r#"["a@x"]"#).unwrap();
    let e = run(ins, r#"["a@x"]"#).unwrap_err();
    assert_eq!(
        e,
        "SQLITE_CONSTRAINT_UNIQUE: UNIQUE constraint failed: u.email"
    );
    assert!(run(ins, "[null]")
        .unwrap_err()
        .starts_with("SQLITE_CONSTRAINT_NOTNULL"));
    assert_eq!(count(), 2);
    let e = text(last_error(db).unwrap());
    assert!(e.starts_with("SQLITE_CONSTRAINT_NOTNULL"), "{e}");
    assert!(prepare(db, "SELEC 1")
        .unwrap_err()
        .starts_with("SQLITE_ERROR: near \"SELEC\""));
    let e = prepare(db, "SELECT 1; SELECT 2").unwrap_err();
    assert!(
        e.starts_with("SQLITE_MISUSE") && e.contains("more than one"),
        "{e}"
    );
    assert_eq!(count(), 4);
    reset_error_count(db, 1);
    assert_eq!(count(), 1);
    unsafe { statement::free(ins) };
    close(db);
}

#[test]
fn statements_fail_cleanly_after_the_database_closes() {
    let db = open_memory();
    let stmt = prepare(db, "SELECT 1 AS one").unwrap();
    assert_eq!(query(stmt, "", true).unwrap(), r#"{"one":1}"#);
    close(db);
    let e = query(stmt, "", true).unwrap_err();
    assert_eq!(e, "SQLITE_MISUSE: The database connection is not open");
    // Null handles are no-ops.
    unsafe { statement::free(stmt) };
    unsafe { statement::free(NULL_STMT) };
    close(NULL_DB);
    assert!(exec(NULL_DB, "SELECT 1").unwrap_err().contains("not open"));
    // `Database` is a Copy struct: another copy of a closed handle fails cleanly, and closing
    // it again is a no-op (it used to reach the freed connection).
    close(db);
    assert!(exec(db, "SELECT 1").unwrap_err().contains("not open"));
    assert!(run(NULL_STMT, "")
        .unwrap_err()
        .contains("statement is not open"));
}

#[test]
fn pragma_and_transaction_state() {
    let db = open_memory();
    let pragma = |source: &str| pragma(db, source).map_err(text);
    assert_eq!(pragma("journal_mode = WAL").unwrap(), "memory");
    assert_eq!(pragma("user_version = 7").unwrap(), "");
    assert_eq!(pragma("user_version").unwrap(), "7");
    let in_tx = || in_transaction(db);
    assert!(!in_tx());
    exec(db, "BEGIN").unwrap();
    assert!(in_tx());
    exec(db, "COMMIT").unwrap();
    assert!(!in_tx());
    close(db);
}

#[test]
fn open_reports_missing_files() {
    let path = std::env::temp_dir().join("velt-sqlite-missing-dir/none.db");
    let e = text(open(&path.to_string_lossy(), false, false, 0).unwrap_err());
    assert!(e.starts_with("SQLITE_CANTOPEN"), "{e}");
}
