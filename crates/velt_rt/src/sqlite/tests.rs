//! ABI-level tests of `velt_rt_sqlite_*`: binding, row encoding, errors and handle lifetimes.

use super::connection::*;
use super::error::code_name;
use super::statement::*;
use crate::result::{IoResult, VeltErr};
use crate::str::VeltStr;
use std::mem::MaybeUninit;

fn s(text: &str) -> VeltStr {
    VeltStr::from_bytes(text.as_bytes())
}

unsafe fn take(v: VeltStr) -> String {
    let t = String::from_utf8_lossy(v.as_bytes()).into_owned();
    let mut v = v;
    crate::str::velt_rt_str_drop(&mut v);
    t
}

/// `Ok(value)` or `Err("SQLITE_NAME: message")`.
unsafe fn result<T>(r: IoResult<T>) -> Result<T, String> {
    if r.err.code == 0 {
        Ok(r.value.assume_init())
    } else {
        Err(err_text(r.err))
    }
}

unsafe fn err_text(e: VeltErr) -> String {
    format!("{}: {}", code_name(e.code), take(e.message))
}

unsafe fn status(e: VeltErr) -> Result<(), String> {
    if e.code == 0 {
        Ok(())
    } else {
        Err(err_text(e))
    }
}

fn open_memory() -> DbHandle {
    let mut out = MaybeUninit::uninit();
    // SAFETY: valid arguments; the result is read according to its code.
    unsafe {
        velt_rt_sqlite_open(&s(":memory:"), 0, 1, 1000, out.as_mut_ptr());
        result(out.assume_init()).expect("open :memory:")
    }
}

fn exec(db: DbHandle, sql: &str) -> Result<(), String> {
    let mut out = MaybeUninit::uninit();
    // SAFETY: valid handle (or null) and string.
    unsafe {
        velt_rt_sqlite_exec(db, &s(sql), out.as_mut_ptr());
        status(out.assume_init())
    }
}

fn prepare(db: DbHandle, sql: &str) -> Result<StmtHandle, String> {
    let mut out = MaybeUninit::uninit();
    // SAFETY: as above.
    unsafe {
        velt_rt_sqlite_prepare(db, &s(sql), out.as_mut_ptr());
        result(out.assume_init())
    }
}

fn run(stmt: StmtHandle, params: &str) -> Result<(i64, i64), String> {
    let mut out = MaybeUninit::uninit();
    // SAFETY: as above.
    unsafe {
        velt_rt_sqlite_run(stmt, &s(params), out.as_mut_ptr());
        result(out.assume_init()).map(|r| (r.changes, r.last_insert_rowid))
    }
}

fn query(stmt: StmtHandle, params: &str, first_only: bool) -> Result<String, String> {
    let mut out = MaybeUninit::uninit();
    // SAFETY: as above.
    unsafe {
        velt_rt_sqlite_query(stmt, &s(params), first_only as u8, out.as_mut_ptr());
        result(out.assume_init()).map(|v| take(v))
    }
}

fn sql_query(db: DbHandle, sql: &str, params: &str) -> Result<String, String> {
    let stmt = prepare(db, sql)?;
    let r = query(stmt, params, false);
    // SAFETY: a live statement handle, released once.
    unsafe { velt_rt_sqlite_stmt_free(stmt) };
    r
}

fn close(db: DbHandle) {
    let mut out = MaybeUninit::uninit();
    // SAFETY: a live database handle, released once.
    unsafe {
        velt_rt_sqlite_close(db, out.as_mut_ptr());
        status(out.assume_init()).expect("close");
    }
}

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
    // SAFETY: live handles, each released once.
    unsafe {
        for h in [ins, by_pos, sel, one] {
            velt_rt_sqlite_stmt_free(h);
        }
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
            r#"{"i":-9007199254740993,"f":1e300,"t":"","b":[],"n":1,"ok":false}]"#
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
    // SAFETY: a live handle, released once.
    unsafe { velt_rt_sqlite_stmt_free(ins) };
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
    // SAFETY: live handles, each released once.
    unsafe {
        velt_rt_sqlite_stmt_free(named);
        velt_rt_sqlite_stmt_free(pos);
    }
    close(db);
}

#[test]
fn sqlite_errors_carry_codes_and_are_counted() {
    let db = open_memory();
    // SAFETY: a live handle.
    let count = || unsafe { velt_rt_sqlite_error_count(db) };
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
    let mut last = MaybeUninit::uninit();
    // SAFETY: a live handle.
    unsafe {
        velt_rt_sqlite_last_error(db, last.as_mut_ptr());
        let e = status(last.assume_init()).unwrap_err();
        assert!(e.starts_with("SQLITE_CONSTRAINT_NOTNULL"), "{e}");
    }
    assert!(prepare(db, "SELEC 1")
        .unwrap_err()
        .starts_with("SQLITE_ERROR: near \"SELEC\""));
    let e = prepare(db, "SELECT 1; SELECT 2").unwrap_err();
    assert!(
        e.starts_with("SQLITE_MISUSE") && e.contains("more than one"),
        "{e}"
    );
    assert_eq!(count(), 4);
    // SAFETY: a live handle.
    unsafe { velt_rt_sqlite_reset_error_count(db, 1) };
    assert_eq!(count(), 1);
    // SAFETY: a live handle, released once.
    unsafe { velt_rt_sqlite_stmt_free(ins) };
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
    // SAFETY: a live handle, released once; null handles are no-ops.
    unsafe {
        velt_rt_sqlite_stmt_free(stmt);
        velt_rt_sqlite_stmt_free(StmtHandle::NULL);
    }
    close(DbHandle::NULL);
    assert!(exec(DbHandle::NULL, "SELECT 1")
        .unwrap_err()
        .contains("not open"));
    // `Database` is a Copy struct: another copy of a closed handle fails cleanly, and closing
    // it again is a no-op (it used to reach the freed connection).
    close(db);
    assert!(exec(db, "SELECT 1").unwrap_err().contains("not open"));
    assert!(run(StmtHandle::NULL, "")
        .unwrap_err()
        .contains("statement is not open"));
}

#[test]
fn pragma_and_transaction_state() {
    let db = open_memory();
    let pragma = |source: &str| {
        let mut out = MaybeUninit::uninit();
        // SAFETY: a live handle and a valid string.
        unsafe {
            velt_rt_sqlite_pragma(db, &s(source), out.as_mut_ptr());
            result(out.assume_init()).map(|v| take(v))
        }
    };
    assert_eq!(pragma("journal_mode = WAL").unwrap(), "memory");
    assert_eq!(pragma("user_version = 7").unwrap(), "");
    assert_eq!(pragma("user_version").unwrap(), "7");
    // SAFETY: a live handle.
    let in_tx = || unsafe { velt_rt_sqlite_in_transaction(db) };
    assert_eq!(in_tx(), 0);
    exec(db, "BEGIN").unwrap();
    assert_eq!(in_tx(), 1);
    exec(db, "COMMIT").unwrap();
    assert_eq!(in_tx(), 0);
    close(db);
}

#[test]
fn open_reports_missing_files() {
    let mut out = MaybeUninit::uninit();
    let path = std::env::temp_dir().join("velt-sqlite-missing-dir/none.db");
    let path = path.to_string_lossy().into_owned();
    // SAFETY: valid arguments.
    unsafe {
        velt_rt_sqlite_open(&s(&path), 0, 0, 0, out.as_mut_ptr());
        let e = result(out.assume_init()).unwrap_err();
        assert!(e.starts_with("SQLITE_CANTOPEN"), "{e}");
    }
}
