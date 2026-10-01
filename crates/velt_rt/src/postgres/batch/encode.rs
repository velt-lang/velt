//! A batch's frontend messages: `Close` / `Parse` of its statement when needed, then `Bind` +
//! `Execute` per parameter set (unnamed portal, every result column in binary, as
//! tokio-postgres asks for them), then one `Sync`.

use crate::postgres::bind::PgParam;
use crate::postgres::error::PgError;
use bytes::BytesMut;
use postgres_protocol::message::frontend;
use postgres_protocol::IsNull;
use tokio_postgres::types::Type;

/// Binary format code (text is 0).
const BINARY: i16 = 1;

fn encoding(e: impl std::fmt::Display) -> PgError {
    PgError::invalid(format!("cannot encode the batch: {e}"))
}

/// `Close` of statement `name` (no error if it does not exist).
pub fn close(name: &str, buf: &mut BytesMut) -> Result<(), PgError> {
    frontend::close(b'S', name, buf).map_err(encoding)
}

/// `Parse` of `sql` as statement `name` with the parameter types tokio-postgres resolved.
pub fn parse(name: &str, sql: &str, types: &[Type], buf: &mut BytesMut) -> Result<(), PgError> {
    frontend::parse(name, sql, types.iter().map(Type::oid), buf).map_err(encoding)
}

/// `Bind` of statement `name` with `values`, then `Execute` of all its rows.
pub fn execution(name: &str, values: &[PgParam], buf: &mut BytesMut) -> Result<(), PgError> {
    let formats = values.iter().map(|v| match v {
        PgParam::Text(_) => 0,
        _ => BINARY,
    });
    let r = frontend::bind(
        "",
        name,
        formats,
        values,
        |v, out| {
            Ok(match v {
                PgParam::Null => IsNull::Yes,
                PgParam::Binary(b) => {
                    out.extend_from_slice(b);
                    IsNull::No
                }
                PgParam::Text(t) => {
                    out.extend_from_slice(t.as_bytes());
                    IsNull::No
                }
            })
        },
        Some(BINARY),
        buf,
    );
    r.map_err(|e| match e {
        frontend::BindError::Conversion(e) => encoding(e),
        frontend::BindError::Serialization(e) => encoding(e),
    })?;
    frontend::execute("", 0, buf).map_err(encoding)
}

/// The group's one `Sync`.
pub fn sync(buf: &mut BytesMut) {
    frontend::sync(buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executions_bind_each_value_in_its_format() {
        let mut buf = BytesMut::new();
        let values = [
            PgParam::Binary(vec![0, 0, 0, 7]),
            PgParam::Text("2024-01-02".into()),
            PgParam::Null,
        ];
        execution("velt_b0", &values, &mut buf).unwrap();
        sync(&mut buf);
        let b = &buf[..];
        assert_eq!(b[0], b'B');
        let bind_len = u32::from_be_bytes([b[1], b[2], b[3], b[4]]) as usize;
        let body = &b[5..1 + bind_len];
        // Portal "" and statement name, NUL-terminated.
        assert!(body.starts_with(b"\0velt_b0\0"));
        let rest = &body[9..];
        // Three format codes: binary, text, binary.
        assert_eq!(&rest[..8], &[0, 3, 0, 1, 0, 0, 0, 1]);
        // Three values: 4 bytes, 10 bytes, NULL (-1).
        assert_eq!(&rest[8..14], &[0, 3, 0, 0, 0, 4]);
        assert_eq!(&rest[18..22], &[0, 0, 0, 10]);
        assert_eq!(&rest[32..36], &[255; 4]);
        // One result format: binary.
        assert_eq!(&rest[36..], &[0, 1, 0, 1]);
        let tail = &b[1 + bind_len..];
        assert_eq!(tail[0], b'E');
        assert_eq!(&tail[tail.len() - 5..], &[b'S', 0, 0, 0, 4]);
    }
}
