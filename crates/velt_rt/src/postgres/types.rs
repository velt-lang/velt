//! Column values (PostgreSQL's binary result format) as JSON, for std's `JSON.parse<T[]>`:
//!
//! | PostgreSQL | JSON | decodes into |
//! |---|---|---|
//! | `int2` `int4` `int8` `oid` | exact integer | integer fields (all of i64), `f64` |
//! | `float4` `float8` | number (`NaN`/`±Infinity` ⇒ `null`) | `f64` |
//! | `numeric` | string of its exact digits | `string` (cast `::float8` for a number) |
//! | `bool` | `true` / `false` | `bool` |
//! | `text` `varchar` `char(n)` `name` `citext`, enums | string | `string` |
//! | `uuid` | `"xxxxxxxx-xxxx-…"` | `string` |
//! | `date` `time` `timestamp` `timestamptz` | ISO 8601 string (see [`super::temporal`]) | `string` |
//! | `interval` | PostgreSQL's text form, `"1 year 2 mons 3 days 04:05:06"` | `string` |
//! | `money` | string of the exact amount, `"-1234.50"` | `string` |
//! | `inet` `cidr` `macaddr` `macaddr8` | PostgreSQL's text form ([`super::network`]) | `string` |
//! | `json` `jsonb` | the document itself | any matching type |
//! | `bytea` | array of byte values | `u8[]` |
//! | arrays of the above | (nested) arrays, `null` elements | `T[]` |
//! | domains | as their base type | |
//! | NULL | `null` | `T \| null` |
//!
//! Anything else (ranges, composites, geometric types …) is an `ENOTSUP` error that names the
//! column and suggests a `::text` cast.

use super::{network, temporal};
use crate::db_json::push_byte_array;
use crate::json::escape::push_json_string;
use tokio_postgres::types::{Kind, Type};

/// OID of `json`.
pub const JSON: u32 = 114;
/// OID of `jsonb`.
pub const JSONB: u32 = 3802;

/// Types whose binary form is their UTF-8 text.
pub fn is_texty(ty: &Type) -> bool {
    matches!(
        *ty,
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::CHAR | Type::UNKNOWN
    ) || matches!(ty.kind(), Kind::Enum(_))
        || ty.name() == "citext"
}

fn be<const N: usize>(raw: &[u8]) -> Result<[u8; N], String> {
    <[u8; N]>::try_from(raw).map_err(|_| format!("expected {N} bytes, got {}", raw.len()))
}

fn push_uuid(out: &mut Vec<u8>, raw: &[u8]) -> Result<(), String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let b: [u8; 16] = be(raw)?;
    out.push(b'"');
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push(b'-');
        }
        out.push(HEX[(byte >> 4) as usize]);
        out.push(HEX[(byte & 15) as usize]);
    }
    out.push(b'"');
    Ok(())
}

fn push_float(out: &mut Vec<u8>, v: f64) {
    if v.is_finite() {
        crate::fmt::push_f64(out, v);
    } else {
        out.extend_from_slice(b"null");
    }
}

fn push_text(out: &mut Vec<u8>, raw: &[u8]) {
    match std::str::from_utf8(raw) {
        Ok(s) => push_json_string(out, s.as_bytes()),
        Err(_) => push_json_string(out, String::from_utf8_lossy(raw).as_bytes()),
    }
}

/// Append the JSON of one non-NULL value of type `ty`. The error names what is unsupported
/// (the caller adds the column).
pub fn push_value(out: &mut Vec<u8>, ty: &Type, raw: &[u8]) -> Result<(), String> {
    let text = |out: &mut Vec<u8>, s: Result<String, String>| {
        s.map(|s| push_json_string(out, s.as_bytes()))
    };
    match *ty {
        Type::BOOL => out.extend_from_slice(if raw.first() == Some(&0) {
            b"false"
        } else {
            b"true"
        }),
        Type::INT2 => crate::fmt::push_i64(out, i16::from_be_bytes(be(raw)?) as i64),
        Type::INT4 => crate::fmt::push_i64(out, i32::from_be_bytes(be(raw)?) as i64),
        Type::INT8 => crate::fmt::push_i64(out, i64::from_be_bytes(be(raw)?)),
        Type::OID => crate::fmt::push_u64(out, u32::from_be_bytes(be(raw)?) as u64),
        Type::FLOAT4 => push_float(out, f32::from_be_bytes(be(raw)?) as f64),
        Type::FLOAT8 => push_float(out, f64::from_be_bytes(be(raw)?)),
        Type::NUMERIC => text(out, temporal::numeric(raw))?,
        Type::UUID => push_uuid(out, raw)?,
        Type::DATE => text(out, temporal::date(raw))?,
        Type::TIME => text(out, temporal::time(raw))?,
        Type::TIMESTAMP => text(out, temporal::timestamp(raw, false))?,
        Type::TIMESTAMPTZ => text(out, temporal::timestamp(raw, true))?,
        Type::INTERVAL => text(out, temporal::interval(raw))?,
        Type::MONEY => text(out, temporal::money(raw))?,
        Type::INET | Type::CIDR => text(out, network::inet(raw))?,
        Type::MACADDR | Type::MACADDR8 => text(out, network::macaddr(raw))?,
        Type::BYTEA => push_byte_array(out, raw),
        _ if ty.oid() == JSON => out.extend_from_slice(raw),
        // jsonb's binary form is a version byte (1) and the text.
        _ if ty.oid() == JSONB => out.extend_from_slice(raw.get(1..).unwrap_or_default()),
        _ if is_texty(ty) => push_text(out, raw),
        _ => match ty.kind() {
            Kind::Array(elem) => push_array(out, elem, raw)?,
            Kind::Domain(base) => push_value(out, base, raw)?,
            _ => return Err(format!("type {}", ty.name())),
        },
    }
    Ok(())
}

/// Whether [`push_value`] can decode values of `ty` (checked per column before any row, so an
/// unsupported column fails even when it is NULL or the result is empty).
pub fn decodable(ty: &Type) -> bool {
    matches!(
        *ty,
        Type::BOOL
            | Type::INT2
            | Type::INT4
            | Type::INT8
            | Type::OID
            | Type::FLOAT4
            | Type::FLOAT8
            | Type::NUMERIC
            | Type::UUID
            | Type::DATE
            | Type::TIME
            | Type::TIMESTAMP
            | Type::TIMESTAMPTZ
            | Type::INTERVAL
            | Type::MONEY
            | Type::INET
            | Type::CIDR
            | Type::MACADDR
            | Type::MACADDR8
            | Type::BYTEA
    ) || ty.oid() == JSON
        || ty.oid() == JSONB
        || is_texty(ty)
        || match ty.kind() {
            Kind::Array(elem) | Kind::Domain(elem) => decodable(elem),
            _ => false,
        }
}

/// Reads the big-endian `i32`s of an array header.
struct Reader<'a> {
    raw: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn i32(&mut self) -> Result<i32, String> {
        let b = self
            .raw
            .get(self.pos..self.pos + 4)
            .ok_or("truncated array")?;
        self.pos += 4;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn element(&mut self) -> Result<Option<&'a [u8]>, String> {
        let len = self.i32()?;
        if len < 0 {
            return Ok(None);
        }
        let end = self.pos + len as usize;
        let b = self.raw.get(self.pos..end).ok_or("truncated array")?;
        self.pos = end;
        Ok(Some(b))
    }
}

/// The binary array format: dimensions, then every element (length-prefixed, -1 = NULL) in
/// row-major order, written as nested JSON arrays.
fn push_array(out: &mut Vec<u8>, elem: &Type, raw: &[u8]) -> Result<(), String> {
    let mut r = Reader { raw, pos: 0 };
    let ndim = r.i32()?;
    r.i32()?; // has-nulls flag
    r.i32()?; // element OID
    let mut dims = Vec::with_capacity(ndim.max(0) as usize);
    for _ in 0..ndim {
        dims.push(r.i32()?.max(0) as usize);
        r.i32()?; // lower bound
    }
    if dims.is_empty() {
        out.extend_from_slice(b"[]");
        return Ok(());
    }
    push_dim(out, elem, &dims, &mut r)
}

fn push_dim(out: &mut Vec<u8>, elem: &Type, dims: &[usize], r: &mut Reader) -> Result<(), String> {
    out.push(b'[');
    for i in 0..dims[0] {
        if i > 0 {
            out.push(b',');
        }
        if dims.len() > 1 {
            push_dim(out, elem, &dims[1..], r)?;
        } else {
            match r.element()? {
                Some(v) => push_value(out, elem, v)?,
                None => out.extend_from_slice(b"null"),
            }
        }
    }
    out.push(b']');
    Ok(())
}
