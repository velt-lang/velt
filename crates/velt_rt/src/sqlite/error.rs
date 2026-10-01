//! SQLite errors at the ABI: a [`DbError`] is an SQLite (extended) result code plus a message,
//! written into the `VeltErr` header of an `IoResult` (`code` != 0 is the failure). std turns the
//! code into `SqliteError.code` ("SQLITE_CONSTRAINT_UNIQUE", …) with `velt_rt_sqlite_error_name`.
//!
//! Failures that are not SQLite's own (bad parameters, a closed handle, several statements in
//! one `prepare`) use the SQLite code that describes them best (`SQLITE_RANGE`,
//! `SQLITE_MISUSE`, `SQLITE_MISMATCH`), as better-sqlite3 does.

use crate::result::VeltErr;
use crate::str::VeltStr;
use rusqlite::ffi;

/// A failed SQLite operation: extended result code (never 0) and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbError {
    /// SQLite extended result code (`SQLITE_CONSTRAINT_UNIQUE` = 2067 …).
    pub code: i32,
    /// Human-readable message (SQLite's `sqlite3_errmsg` where there is one).
    pub message: String,
}

impl DbError {
    /// An error with an explicit code.
    pub fn new(code: i32, message: impl Into<String>) -> DbError {
        DbError {
            code,
            message: message.into(),
        }
    }

    /// A parameter problem (missing, extra or badly typed values): `SQLITE_RANGE`.
    pub fn range(message: impl Into<String>) -> DbError {
        DbError::new(ffi::SQLITE_RANGE, message)
    }

    /// Use of a closed database or statement: `SQLITE_MISUSE`.
    pub fn closed(what: &str) -> DbError {
        DbError::new(ffi::SQLITE_MISUSE, format!("The {what} is not open"))
    }

    /// The `VeltErr` header for this error.
    pub fn to_velt(&self) -> VeltErr {
        VeltErr::new(self.code, &self.message)
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> DbError {
        let code = match &e {
            rusqlite::Error::MultipleStatement => ffi::SQLITE_MISUSE,
            rusqlite::Error::InvalidParameterName(_)
            | rusqlite::Error::InvalidParameterCount(..) => ffi::SQLITE_RANGE,
            rusqlite::Error::Utf8Error(..) | rusqlite::Error::NulError(_) => ffi::SQLITE_MISMATCH,
            other => other
                .sqlite_error()
                .map_or(ffi::SQLITE_ERROR, |f| f.extended_code),
        };
        let message = match e {
            rusqlite::Error::MultipleStatement => {
                "The supplied SQL string contains more than one statement".to_string()
            }
            other => other.to_string(),
        };
        DbError::new(if code == 0 { ffi::SQLITE_ERROR } else { code }, message)
    }
}

/// The name of an SQLite result code: the extended name when it has one of the common ones
/// (`SQLITE_CONSTRAINT_UNIQUE`, `SQLITE_BUSY_SNAPSHOT` …), else the primary name.
pub fn code_name(code: i32) -> &'static str {
    extended_name(code).unwrap_or_else(|| primary_name(code & 0xff))
}

fn extended_name(code: i32) -> Option<&'static str> {
    Some(match code {
        ffi::SQLITE_CONSTRAINT_CHECK => "SQLITE_CONSTRAINT_CHECK",
        ffi::SQLITE_CONSTRAINT_FOREIGNKEY => "SQLITE_CONSTRAINT_FOREIGNKEY",
        ffi::SQLITE_CONSTRAINT_NOTNULL => "SQLITE_CONSTRAINT_NOTNULL",
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY => "SQLITE_CONSTRAINT_PRIMARYKEY",
        ffi::SQLITE_CONSTRAINT_TRIGGER => "SQLITE_CONSTRAINT_TRIGGER",
        ffi::SQLITE_CONSTRAINT_UNIQUE => "SQLITE_CONSTRAINT_UNIQUE",
        ffi::SQLITE_CONSTRAINT_ROWID => "SQLITE_CONSTRAINT_ROWID",
        ffi::SQLITE_CONSTRAINT_DATATYPE => "SQLITE_CONSTRAINT_DATATYPE",
        ffi::SQLITE_BUSY_RECOVERY => "SQLITE_BUSY_RECOVERY",
        ffi::SQLITE_BUSY_SNAPSHOT => "SQLITE_BUSY_SNAPSHOT",
        ffi::SQLITE_BUSY_TIMEOUT => "SQLITE_BUSY_TIMEOUT",
        ffi::SQLITE_LOCKED_SHAREDCACHE => "SQLITE_LOCKED_SHAREDCACHE",
        ffi::SQLITE_READONLY_DBMOVED => "SQLITE_READONLY_DBMOVED",
        ffi::SQLITE_CANTOPEN_ISDIR => "SQLITE_CANTOPEN_ISDIR",
        ffi::SQLITE_CANTOPEN_FULLPATH => "SQLITE_CANTOPEN_FULLPATH",
        _ => return None,
    })
}

fn primary_name(code: i32) -> &'static str {
    const NAMES: [&str; 29] = [
        "SQLITE_OK",
        "SQLITE_ERROR",
        "SQLITE_INTERNAL",
        "SQLITE_PERM",
        "SQLITE_ABORT",
        "SQLITE_BUSY",
        "SQLITE_LOCKED",
        "SQLITE_NOMEM",
        "SQLITE_READONLY",
        "SQLITE_INTERRUPT",
        "SQLITE_IOERR",
        "SQLITE_CORRUPT",
        "SQLITE_NOTFOUND",
        "SQLITE_FULL",
        "SQLITE_CANTOPEN",
        "SQLITE_PROTOCOL",
        "SQLITE_EMPTY",
        "SQLITE_SCHEMA",
        "SQLITE_TOOBIG",
        "SQLITE_CONSTRAINT",
        "SQLITE_MISMATCH",
        "SQLITE_MISUSE",
        "SQLITE_NOLFS",
        "SQLITE_AUTH",
        "SQLITE_FORMAT",
        "SQLITE_RANGE",
        "SQLITE_NOTADB",
        "SQLITE_NOTICE",
        "SQLITE_WARNING",
    ];
    usize::try_from(code)
        .ok()
        .and_then(|i| NAMES.get(i))
        .copied()
        .unwrap_or("SQLITE_ERROR")
}

/// Write the name of SQLite result `code` (a static string: nothing to free) to `out`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_error_name(code: i32, out: *mut VeltStr) {
    out.write(VeltStr::from_static(code_name(code).as_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_prefer_known_extended_codes() {
        assert_eq!(
            code_name(ffi::SQLITE_CONSTRAINT_UNIQUE),
            "SQLITE_CONSTRAINT_UNIQUE"
        );
        assert_eq!(code_name(ffi::SQLITE_CONSTRAINT), "SQLITE_CONSTRAINT");
        // An extended code without its own entry falls back to its primary code.
        assert_eq!(code_name(ffi::SQLITE_IOERR_READ), "SQLITE_IOERR");
        assert_eq!(code_name(ffi::SQLITE_RANGE), "SQLITE_RANGE");
        assert_eq!(code_name(-5), "SQLITE_ERROR");
    }

    #[test]
    fn rusqlite_errors_map_to_codes() {
        let e = DbError::from(rusqlite::Error::MultipleStatement);
        assert_eq!(e.code, ffi::SQLITE_MISUSE);
        assert!(e.message.contains("more than one statement"));
        let e = DbError::from(rusqlite::Error::InvalidParameterName(":x".into()));
        assert_eq!(e.code, ffi::SQLITE_RANGE);
        assert_eq!(
            DbError::from(rusqlite::Error::QueryReturnedNoRows).code,
            ffi::SQLITE_ERROR
        );
    }
}
