//! Parameters: JSON values ([`DbValue`], from `crate::db_json`) converted for the parameter
//! types the server inferred when it prepared the statement.
//!
//! Common types are sent in binary: integers (range-checked: `70000` for an `int2` is an error
//! here, not a silent wrap), floats, `bool`, text-like types and `bytea`, plus `json`/`jsonb`
//! from a string holding JSON. A string for any other type (`uuid`, `numeric`, `date`,
//! `timestamptz`, `interval`, arrays such as `'{1,2}'`, enums …) is sent in PostgreSQL's text
//! format and parsed by the server, which reports bad input with its own SQLSTATE (`22P02` …).
//! Numbers also convert to `numeric` and to text-like types; booleans to text-like types.
//! Anything else (a number for a `uuid`, a `u8[]` for an `int4` …) is a clear `EINVAL`.

use super::error::PgError;
use super::types::{is_texty, JSON, JSONB};
use crate::db_json::{DbValue, Params};
use bytes::BytesMut;
use std::error::Error as StdError;
use tokio_postgres::types::{to_sql_checked, Format, IsNull, ToSql, Type};

/// One parameter, already encoded for its type.
#[derive(Debug, Clone, PartialEq)]
pub enum PgParam {
    /// SQL NULL.
    Null,
    /// The binary encoding.
    Binary(Vec<u8>),
    /// PostgreSQL's text input format (the server parses it).
    Text(String),
}

impl ToSql for PgParam {
    fn to_sql(
        &self,
        _: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn StdError + Sync + Send>> {
        match self {
            PgParam::Null => return Ok(IsNull::Yes),
            PgParam::Binary(b) => out.extend_from_slice(b),
            PgParam::Text(t) => out.extend_from_slice(t.as_bytes()),
        }
        Ok(IsNull::No)
    }

    fn accepts(_: &Type) -> bool {
        // Checked by `convert`, which knows the value too.
        true
    }

    fn encode_format(&self, _: &Type) -> Format {
        match self {
            PgParam::Text(_) => Format::Text,
            _ => Format::Binary,
        }
    }

    to_sql_checked!();
}

fn kind_of(v: &DbValue) -> &'static str {
    match v {
        DbValue::Null => "null",
        DbValue::Bool(_) => "a bool",
        DbValue::Int(_) => "an integer",
        DbValue::Float(_) => "a float",
        DbValue::Text(_) => "a string",
        DbValue::Bytes(_) => "a u8[]",
    }
}

/// A number in JavaScript's shortest form.
fn float_text(f: f64) -> String {
    let mut v = Vec::new();
    crate::fmt::push_f64(&mut v, f);
    String::from_utf8_lossy(&v).into_owned()
}

fn int_param(i: i64, ty: &Type) -> Result<PgParam, String> {
    let range = || format!("{i} is out of range for {}", ty.name());
    Ok(PgParam::Binary(match *ty {
        Type::INT2 => i16::try_from(i)
            .map_err(|_| range())?
            .to_be_bytes()
            .to_vec(),
        Type::INT4 => i32::try_from(i)
            .map_err(|_| range())?
            .to_be_bytes()
            .to_vec(),
        Type::OID => u32::try_from(i)
            .map_err(|_| range())?
            .to_be_bytes()
            .to_vec(),
        Type::INT8 => i.to_be_bytes().to_vec(),
        Type::FLOAT4 => (i as f32).to_be_bytes().to_vec(),
        Type::FLOAT8 => (i as f64).to_be_bytes().to_vec(),
        Type::NUMERIC => return Ok(PgParam::Text(i.to_string())),
        _ => return Err(String::new()),
    }))
}

fn float_param(f: f64, ty: &Type) -> Result<PgParam, String> {
    Ok(match *ty {
        Type::FLOAT4 => PgParam::Binary((f as f32).to_be_bytes().to_vec()),
        Type::FLOAT8 => PgParam::Binary(f.to_be_bytes().to_vec()),
        Type::NUMERIC if f.is_finite() => PgParam::Text(float_text(f)),
        _ => return Err(String::new()),
    })
}

fn text_param(s: &str, ty: &Type) -> PgParam {
    if *ty == Type::JSONB {
        let mut b = Vec::with_capacity(s.len() + 1);
        b.push(1); // jsonb binary format version
        b.extend_from_slice(s.as_bytes());
        return PgParam::Binary(b);
    }
    if is_texty(ty) || ty.oid() == JSON {
        PgParam::Binary(s.as_bytes().to_vec())
    } else {
        PgParam::Text(s.to_string())
    }
}

/// Encode `v` for a parameter of type `ty`.
pub fn convert(v: &DbValue, ty: &Type) -> Result<PgParam, String> {
    let json = ty.oid() == JSON || ty.oid() == JSONB;
    let r = match v {
        DbValue::Null => Ok(PgParam::Null),
        DbValue::Text(s) => Ok(text_param(s, ty)),
        DbValue::Int(i) if is_texty(ty) || json => Ok(text_param(&i.to_string(), ty)),
        DbValue::Float(f) if is_texty(ty) || json => Ok(text_param(&float_text(*f), ty)),
        DbValue::Bool(b) if is_texty(ty) || json => Ok(text_param(&b.to_string(), ty)),
        DbValue::Int(i) => int_param(*i, ty),
        DbValue::Float(f) => float_param(*f, ty),
        DbValue::Bool(b) if *ty == Type::BOOL => Ok(PgParam::Binary(vec![*b as u8])),
        DbValue::Bytes(b) if *ty == Type::BYTEA => Ok(PgParam::Binary(b.clone())),
        _ => Err(String::new()),
    };
    r.map_err(|e| {
        if e.is_empty() {
            format!(
                "cannot bind {} to a parameter of type {}",
                kind_of(v),
                ty.name()
            )
        } else {
            e
        }
    })
}

/// Every parameter of a statement with parameter types `types`, in `$n` order. `names` are
/// the placeholder names when the SQL was rewritten from named placeholders.
pub fn bind_all(
    params: &Params,
    names: Option<&[String]>,
    types: &[Type],
) -> Result<Vec<PgParam>, PgError> {
    let at = |i: usize, v: &DbValue| {
        convert(v, &types[i]).map_err(|e| PgError::invalid(format!("parameter ${}: {e}", i + 1)))
    };
    match (params, names) {
        (Params::Named(_), Some(names)) => names
            .iter()
            .enumerate()
            .map(|(i, name)| match params.named(name) {
                Some(v) => at(i, v),
                None => Err(PgError::invalid(format!(
                    "missing parameter \"{name}\" (placeholder ${})",
                    i + 1
                ))),
            })
            .collect(),
        (Params::Positional(values), _) => {
            if values.len() != types.len() {
                return Err(PgError::invalid(format!(
                    "the statement takes {} parameter(s) but {} were given",
                    types.len(),
                    values.len()
                )));
            }
            values.iter().enumerate().map(|(i, v)| at(i, v)).collect()
        }
        _ if types.is_empty() => Ok(Vec::new()),
        _ => Err(PgError::invalid(format!(
            "the statement takes {} parameter(s) but none were given",
            types.len()
        ))),
    }
}
