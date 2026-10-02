//! The native half of the `sqlite` package: SQLite (bundled through rusqlite) behind the
//! functions `src/lib.vlt` declares. Connections live in a table keyed by never-reused ids, so a
//! closed or unknown handle is an error, never a dangling pointer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::types::{Value, ValueRef};
use rusqlite::Connection;
use velt_native::{code, export, Error};

velt_native::package!(sqlite);

/// SQLite's "misuse" result code, reported for a closed handle.
const MISUSE: i32 = 21;

type Shared = Arc<Mutex<Connection>>;

fn table() -> &'static Mutex<(u64, HashMap<u64, Shared>)> {
    static T: OnceLock<Mutex<(u64, HashMap<u64, Shared>)>> = OnceLock::new();
    T.get_or_init(|| Mutex::new((0, HashMap::new())))
}

fn conn(db: u64) -> Result<Shared, Error> {
    let t = table().lock().unwrap_or_else(|e| e.into_inner());
    t.1.get(&db)
        .cloned()
        .ok_or_else(|| Error::new(MISUSE, "the database is closed"))
}

fn sql_error(e: rusqlite::Error) -> Error {
    let c = match &e {
        rusqlite::Error::SqliteFailure(f, _) => f.extended_code,
        _ => code::OTHER,
    };
    Error::new(c, e.to_string())
}

#[export]
fn sqlite_open(path: &str) -> Result<u64, Error> {
    let c = Connection::open(path).map_err(sql_error)?;
    let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
    t.0 += 1;
    let id = t.0;
    t.1.insert(id, Arc::new(Mutex::new(c)));
    Ok(id)
}

#[export]
fn sqlite_close(db: u64) -> Result<(), Error> {
    let removed = table()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .1
        .remove(&db);
    match removed {
        Some(_) => Ok(()),
        None => Err(Error::new(MISUSE, "the database is closed")),
    }
}

#[export]
fn sqlite_exec(db: u64, sql: &str) -> Result<(), Error> {
    let c = conn(db)?;
    let c = c.lock().unwrap_or_else(|e| e.into_inner());
    c.execute_batch(sql).map_err(sql_error)
}

fn to_value(v: &serde_json::Value) -> Result<Value, Error> {
    Ok(match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Integer(*b as i64),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Real(n.as_f64().unwrap_or(0.0)),
        },
        serde_json::Value::String(s) => Value::Text(s.clone()),
        other => {
            return Err(Error::new(
                code::INVALID_INPUT,
                format!("cannot bind {other} (use null, a boolean, a number or a string)"),
            ))
        }
    })
}

fn to_json(v: ValueRef) -> serde_json::Value {
    match v {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(i) => i.into(),
        ValueRef::Real(f) => f.into(),
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned().into(),
        ValueRef::Blob(b) => b.iter().map(|&x| serde_json::Value::from(x)).collect(),
    }
}

/// Rows as a JSON array of objects (column name → value); `params` is a JSON array.
fn query(db: u64, sql: &str, params: &str) -> Result<String, Error> {
    let params: Vec<serde_json::Value> = serde_json::from_str(params)
        .map_err(|e| Error::new(code::INVALID_INPUT, format!("parameters: {e}")))?;
    let values = params.iter().map(to_value).collect::<Result<Vec<_>, _>>()?;
    let c = conn(db)?;
    let c = c.lock().unwrap_or_else(|e| e.into_inner());
    let mut stmt = c.prepare(sql).map_err(sql_error)?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt
        .query(rusqlite::params_from_iter(values))
        .map_err(sql_error)?;
    let mut out = vec![];
    while let Some(row) = rows.next().map_err(sql_error)? {
        let mut obj = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            obj.insert(name.clone(), to_json(row.get_ref(i).map_err(sql_error)?));
        }
        out.push(serde_json::Value::Object(obj));
    }
    Ok(serde_json::Value::Array(out).to_string())
}

#[export]
fn sqlite_query(db: u64, sql: &str, params: &str) -> Result<String, Error> {
    query(db, sql, params)
}

/// The same query on the runtime's blocking pool (`declare async function`).
#[export(blocking)]
fn sqlite_query_async(db: u64, sql: String, params: String) -> Result<String, Error> {
    query(db, &sql, &params)
}

#[export]
fn sqlite_version() -> String {
    rusqlite::version().to_string()
}

/// SQLite's name for a result code (`SQLITE_CONSTRAINT_UNIQUE`); Velt's own codes (bad
/// parameters) are `SQLITE_ERROR`.
#[export]
fn sqlite_error_name(code: i32) -> String {
    let name = match code {
        2067 => "SQLITE_CONSTRAINT_UNIQUE",
        1555 => "SQLITE_CONSTRAINT_PRIMARYKEY",
        787 => "SQLITE_CONSTRAINT_FOREIGNKEY",
        1299 => "SQLITE_CONSTRAINT_NOTNULL",
        275 => "SQLITE_CONSTRAINT_CHECK",
        c => match c & 0xff {
            5 => "SQLITE_BUSY",
            6 => "SQLITE_LOCKED",
            8 => "SQLITE_READONLY",
            11 => "SQLITE_CORRUPT",
            14 => "SQLITE_CANTOPEN",
            19 => "SQLITE_CONSTRAINT",
            20 => "SQLITE_MISMATCH",
            21 => "SQLITE_MISUSE",
            25 => "SQLITE_RANGE",
            _ => "SQLITE_ERROR",
        },
    };
    name.to_string()
}
