//! PostgreSQL failures at the ABI. A [`PgError`] carries what std's `PgError` class shows: a
//! string `code` (the server's SQLSTATE such as `"23505"`, or a Node-style name for failures
//! outside the server: `"ECONNREFUSED"`, `"EINVAL"` …), the message, and the server's `detail`
//! and `constraint` fields when it sent them.
//!
//! Because the code is a string, the whole error travels as JSON in the `VeltErr` message
//! (`{"code":"23505","message":"…","detail":"…","constraint":"…"}`, absent fields `null`);
//! `VeltErr.code` is only the failure flag (the §3 I/O code when an I/O error caused it, else
//! `OTHER`). std decodes the JSON with `JSON.parse`.

use crate::json::escape::push_json_string;
use crate::result::{code, code_name, VeltErr};
use std::error::Error as StdError;
use std::io;

/// Code of an operation on a closed `Client` or an ended `Pool`.
pub const CLOSED: &str = "ECLOSED";
/// Code of a bad argument: connection string, parameters, placeholder names.
pub const INVALID: &str = "EINVAL";
/// Code of a result column whose type the driver cannot decode.
pub const UNSUPPORTED: &str = "ENOTSUP";
/// Code of a TLS handshake or certificate failure.
pub const TLS: &str = "ETLS";

/// A failed PostgreSQL operation (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgError {
    /// SQLSTATE or Node-style name.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// The server's DETAIL field.
    pub detail: Option<String>,
    /// The violated constraint's name, for constraint errors.
    pub constraint: Option<String>,
    /// The §3 code of the I/O error behind this failure (`OTHER` otherwise).
    io_code: i32,
}

impl PgError {
    /// A failure outside the server with a Node-style `code`.
    pub fn new(code: &str, message: impl Into<String>) -> PgError {
        PgError {
            code: code.to_string(),
            message: message.into(),
            detail: None,
            constraint: None,
            io_code: crate::result::code::OTHER,
        }
    }

    /// A bad argument (`EINVAL`).
    pub fn invalid(message: impl Into<String>) -> PgError {
        PgError::new(INVALID, message)
    }

    /// Use of a closed client or an ended pool (`ECLOSED`).
    pub fn closed(what: &str) -> PgError {
        PgError::new(CLOSED, format!("the {what} is closed"))
    }

    /// An I/O failure, coded with its Node-style name.
    pub fn io(e: &io::Error) -> PgError {
        let velt = VeltErr::from_io(e);
        let mut err = PgError::new(code_name(velt.code), e.to_string());
        err.io_code = velt.code;
        err
    }

    /// The error as JSON text (the `VeltErr` message).
    pub fn to_json(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.message.len() + 64);
        out.extend_from_slice(b"{\"code\":");
        push_json_string(&mut out, self.code.as_bytes());
        out.extend_from_slice(b",\"message\":");
        push_json_string(&mut out, self.message.as_bytes());
        for (key, value) in [("detail", &self.detail), ("constraint", &self.constraint)] {
            out.extend_from_slice(format!(",\"{key}\":").as_bytes());
            match value {
                Some(v) => push_json_string(&mut out, v.as_bytes()),
                None => out.extend_from_slice(b"null"),
            }
        }
        out.push(b'}');
        out
    }

    /// The `VeltErr` header for this error.
    pub fn to_velt(&self) -> VeltErr {
        let flag = if self.io_code == code::OK {
            code::OTHER
        } else {
            self.io_code
        };
        let json = self.to_json();
        // The JSON is built from UTF-8 strings, so it is UTF-8 itself.
        VeltErr::new(flag, std::str::from_utf8(&json).unwrap_or("{}"))
    }
}

/// The first I/O error in `e`'s source chain.
fn io_source<'a>(e: &'a (dyn StdError + 'static)) -> Option<&'a io::Error> {
    let mut next = Some(e);
    while let Some(cur) = next {
        if let Some(io) = cur.downcast_ref::<io::Error>() {
            return Some(io);
        }
        next = cur.source();
    }
    None
}

/// Whether an I/O error wraps a rustls failure (tokio-rustls reports them as `InvalidData`).
fn is_tls(e: &io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.downcast_ref::<rustls::Error>().is_some())
}

impl From<tokio_postgres::Error> for PgError {
    fn from(e: tokio_postgres::Error) -> PgError {
        if let Some(db) = e.as_db_error() {
            return PgError {
                code: db.code().code().to_string(),
                message: db.message().to_string(),
                detail: db.detail().map(str::to_string),
                constraint: db.constraint().map(str::to_string),
                io_code: code::OTHER,
            };
        }
        let kind = e.to_string();
        let message = match e.source() {
            Some(cause) => format!("{kind}: {cause}"),
            None => kind.clone(),
        };
        if e.is_closed() {
            return PgError::new("ECONNRESET", message);
        }
        if kind.starts_with("error performing TLS") {
            return PgError::new(TLS, message);
        }
        if let Some(io) = e.source().and_then(io_source) {
            if is_tls(io) {
                return PgError::new(TLS, message);
            }
            let mut err = PgError::io(io);
            err.message = message;
            return err;
        }
        let code = if kind.starts_with("timeout") {
            "ETIMEDOUT"
        } else if kind.starts_with("invalid connection string")
            || kind.starts_with("invalid configuration")
            || kind.starts_with("error serializing parameter")
            || kind.starts_with("expected ")
        {
            INVALID
        } else {
            "UNKNOWN"
        };
        PgError::new(code, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_has_every_field() {
        let mut e = PgError::new("23505", "dup \"key\"");
        e.constraint = Some("t_pkey".into());
        assert_eq!(
            String::from_utf8(e.to_json()).unwrap(),
            r#"{"code":"23505","message":"dup \"key\"","detail":null,"constraint":"t_pkey"}"#
        );
        assert_eq!(e.to_velt().code, code::OTHER);
    }

    #[test]
    fn io_errors_keep_their_code() {
        let e = PgError::io(&io::Error::from(io::ErrorKind::ConnectionRefused));
        assert_eq!(e.code, "ECONNREFUSED");
        assert_eq!(e.to_velt().code, code::CONNECTION_REFUSED);
    }
}
