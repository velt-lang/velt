//! Server-free tests: placeholder rewriting, parameter conversion, value decoding, connection
//! strings and TLS configuration. Everything that needs a server is covered by the
//! `tests/golden/std/postgres_*` goldens (run with `VELT_TEST_PG_URL`).

use super::bind::{bind_all, convert, PgParam};
use super::config::{parse, Verify};
use super::network::{inet, macaddr};
use super::placeholders::rewrite_named;
use super::temporal::{date, interval, money, numeric, time, timestamp};
use super::types::{decodable, push_value};
use crate::db_json::{parse_params, DbValue};
use tokio_postgres::config::SslMode;
use tokio_postgres::types::Type;

fn rewrite(sql: &str) -> (String, Vec<String>) {
    let r = rewrite_named(sql).unwrap();
    (r.sql, r.names)
}

#[test]
fn named_placeholders_become_positional() {
    let (sql, names) = rewrite("SELECT * FROM t WHERE a = :a AND b = $b OR a = :a");
    assert_eq!(sql, "SELECT * FROM t WHERE a = $1 AND b = $2 OR a = $1");
    assert_eq!(names, ["a", "b"]);
    let (sql, names) = rewrite("INSERT INTO t VALUES (:id,:user_name2)");
    assert_eq!(sql, "INSERT INTO t VALUES ($1,$2)");
    assert_eq!(names, ["id", "user_name2"]);
}

#[test]
fn look_alikes_are_left_alone() {
    let cases = [
        "SELECT x::int, y::text[] FROM t",
        "SELECT ':a', 'it''s :b', E'\\' :c', \"col:d\" FROM t",
        "SELECT $$ :a $b $$, $tag$ :c $tag$ FROM t",
        "SELECT 1 -- :a\n, 2 /* :b /* nested :c */ :d */",
        "SELECT a$b, arr[1:2], arr[lo : hi] FROM t",
    ];
    for sql in cases {
        let (out, names) = rewrite(sql);
        assert_eq!(out, sql);
        assert!(names.is_empty(), "{sql}: {names:?}");
    }
    let (sql, names) = rewrite("SELECT :v::int + 1, ':v'");
    assert_eq!(sql, "SELECT $1::int + 1, ':v'");
    assert_eq!(names, ["v"]);
}

#[test]
fn positional_and_named_do_not_mix() {
    let e = rewrite_named("SELECT :a, $1").unwrap_err();
    assert!(e.contains("cannot be mixed"), "{e}");
}

#[test]
fn values_convert_for_parameter_types() {
    let bin = |v: &[u8]| PgParam::Binary(v.to_vec());
    assert_eq!(convert(&DbValue::Int(7), &Type::INT2), Ok(bin(&[0, 7])));
    assert_eq!(convert(&DbValue::Int(-1), &Type::INT8), Ok(bin(&[255; 8])));
    assert_eq!(
        convert(&DbValue::Int(2), &Type::FLOAT8),
        Ok(bin(&2.0f64.to_be_bytes()))
    );
    assert_eq!(
        convert(&DbValue::Float(0.1), &Type::NUMERIC),
        Ok(PgParam::Text("0.1".into()))
    );
    assert_eq!(convert(&DbValue::Int(5), &Type::TEXT), Ok(bin(b"5")));
    assert_eq!(convert(&DbValue::Bool(true), &Type::BOOL), Ok(bin(&[1])));
    assert_eq!(
        convert(&DbValue::Text("{1,2}".into()), &Type::INT4_ARRAY),
        Ok(PgParam::Text("{1,2}".into()))
    );
    assert_eq!(
        convert(&DbValue::Text("{}".into()), &Type::JSONB),
        Ok(bin(b"\x01{}"))
    );
    assert_eq!(
        convert(&DbValue::Bytes(vec![1, 2]), &Type::BYTEA),
        Ok(bin(&[1, 2]))
    );
    assert_eq!(convert(&DbValue::Null, &Type::UUID), Ok(PgParam::Null));
}

#[test]
fn bad_values_are_rejected_clearly() {
    let e = convert(&DbValue::Int(70000), &Type::INT2).unwrap_err();
    assert_eq!(e, "70000 is out of range for int2");
    let e = convert(&DbValue::Bool(true), &Type::INT4).unwrap_err();
    assert_eq!(e, "cannot bind a bool to a parameter of type int4");
    let e = convert(&DbValue::Int(1), &Type::UUID).unwrap_err();
    assert_eq!(e, "cannot bind an integer to a parameter of type uuid");
    let e = convert(&DbValue::Float(f64::NAN), &Type::NUMERIC).unwrap_err();
    assert!(e.contains("a float"), "{e}");
}

#[test]
fn binding_matches_names_and_counts() {
    let types = [Type::INT4, Type::TEXT];
    let named = parse_params(br#"{"b":"x","a":1,"extra":true}"#).unwrap();
    let names = ["a".to_string(), "b".to_string()];
    let bound = bind_all(&named, Some(&names), &types).unwrap();
    assert_eq!(bound[1], PgParam::Binary(b"x".to_vec()));
    let missing = parse_params(br#"{"a":1}"#).unwrap();
    let e = bind_all(&missing, Some(&names), &types).unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        ("EINVAL", "missing parameter \"b\" (placeholder $2)")
    );
    let short = parse_params(b"[1]").unwrap();
    assert!(bind_all(&short, None, &types)
        .unwrap_err()
        .message
        .contains("takes 2"));
    let none = parse_params(b"").unwrap();
    assert!(bind_all(&none, None, &[]).unwrap().is_empty());
    assert!(bind_all(&none, None, &types).is_err());
}

fn json(ty: &Type, raw: &[u8]) -> String {
    let mut out = Vec::new();
    push_value(&mut out, ty, raw).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn scalars_decode_to_json() {
    assert_eq!(
        json(&Type::INT8, &i64::MIN.to_be_bytes()),
        i64::MIN.to_string()
    );
    assert_eq!(json(&Type::FLOAT4, &1.5f32.to_be_bytes()), "1.5");
    assert_eq!(json(&Type::FLOAT8, &f64::NAN.to_be_bytes()), "null");
    assert_eq!(json(&Type::BOOL, &[0]), "false");
    assert_eq!(json(&Type::TEXT, b"a\"b"), r#""a\"b""#);
    assert_eq!(json(&Type::JSONB, b"\x01{\"a\":1}"), r#"{"a":1}"#);
    assert_eq!(json(&Type::BYTEA, &[0, 255]), "[0,255]");
    let uuid = [
        0xa0, 0xee, 0xbc, 0x99, 0x9c, 0x0b, 0x4e, 0xf8, 0xbb, 0x6d, 0x6b, 0xb9, 0xbd, 0x38, 0x0a,
        0x11,
    ];
    assert_eq!(
        json(&Type::UUID, &uuid),
        r#""a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11""#
    );
}

/// The binary array format for one dimension of int4 values (`None` = NULL).
fn int4_array(values: &[Option<i32>]) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [1, 1, Type::INT4.oid() as i32, values.len() as i32, 1] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    for v in values {
        match v {
            Some(v) => {
                b.extend_from_slice(&4i32.to_be_bytes());
                b.extend_from_slice(&v.to_be_bytes());
            }
            None => b.extend_from_slice(&(-1i32).to_be_bytes()),
        }
    }
    b
}

#[test]
fn arrays_decode_with_nulls() {
    let raw = int4_array(&[Some(1), None, Some(-3)]);
    assert_eq!(json(&Type::INT4_ARRAY, &raw), "[1,null,-3]");
    let empty = [0i32, 0, 23]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect::<Vec<_>>();
    assert_eq!(json(&Type::INT4_ARRAY, &empty), "[]");
    let mut out = Vec::new();
    assert!(push_value(&mut out, &Type::INT4_ARRAY, &raw[..raw.len() - 2]).is_err());
}

#[test]
fn only_known_types_are_decodable() {
    assert!(decodable(&Type::TIMESTAMPTZ) && decodable(&Type::TEXT_ARRAY));
    assert!(decodable(&Type::INTERVAL) && decodable(&Type::INET) && decodable(&Type::MONEY));
    assert!(
        !decodable(&Type::POINT) && !decodable(&Type::INT4_RANGE) && !decodable(&Type::TS_VECTOR)
    );
}

/// `numeric`'s binary form from its fields.
fn numeric_raw(weight: i16, sign: u16, dscale: i16, digits: &[i16]) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [digits.len() as i16, weight, sign as i16, dscale] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    for d in digits {
        b.extend_from_slice(&d.to_be_bytes());
    }
    b
}

#[test]
fn numerics_keep_every_digit() {
    // 12345678.0100 = groups 1234 5678 . 0100, weight 1.
    assert_eq!(
        numeric(&numeric_raw(1, 0, 4, &[1234, 5678, 100])).unwrap(),
        "12345678.0100"
    );
    assert_eq!(
        numeric(&numeric_raw(0, 0x4000, 3, &[5, 1000])).unwrap(),
        "-5.100"
    );
    // 0.00012 = groups 0001 2000 from weight -1; a leading zero group is left out.
    assert_eq!(
        numeric(&numeric_raw(-1, 0, 5, &[1, 2000])).unwrap(),
        "0.00012"
    );
    assert_eq!(
        numeric(&numeric_raw(-2, 0, 8, &[1200])).unwrap(),
        "0.00001200"
    );
    assert_eq!(numeric(&numeric_raw(2, 0, 0, &[7])).unwrap(), "700000000");
    assert_eq!(numeric(&numeric_raw(0, 0, 0, &[])).unwrap(), "0");
    assert_eq!(numeric(&numeric_raw(0, 0xC000, 0, &[])).unwrap(), "NaN");
    assert_eq!(
        numeric(&numeric_raw(0, 0xF000, 0, &[])).unwrap(),
        "-Infinity"
    );
}

#[test]
fn dates_and_times_are_iso() {
    let d = |days: i32| date(&days.to_be_bytes()).unwrap();
    assert_eq!(d(0), "2000-01-01");
    assert_eq!(d(1520), "2004-02-29");
    assert_eq!(d(-730_485), "0000-01-01");
    assert_eq!(d(-730_486), "-000001-12-31");
    assert_eq!(d(i32::MAX), "infinity");
    let us = |v: i64| v.to_be_bytes();
    assert_eq!(time(&us(30_600_123_456)).unwrap(), "08:30:00.123456");
    assert_eq!(time(&us(0)).unwrap(), "00:00:00");
    assert_eq!(
        timestamp(&us(1_500_000), true).unwrap(),
        "2000-01-01T00:00:01.500Z"
    );
    assert_eq!(
        timestamp(&us(-1), false).unwrap(),
        "1999-12-31T23:59:59.999999"
    );
    assert_eq!(timestamp(&us(i64::MIN), true).unwrap(), "-infinity");
}

#[test]
fn connection_strings_and_ssl_modes() {
    let c = parse("postgres://u:p@db.example:6543/app?application_name=x").unwrap();
    assert_eq!(c.inner.get_ssl_mode(), SslMode::Prefer);
    assert_eq!(c.verify, Verify::None);
    let c = parse("postgresql://u@h/d?sslmode=verify-full&sslrootcert=%2Ftmp%2Fca.pem").unwrap();
    assert_eq!(
        (c.inner.get_ssl_mode(), c.verify),
        (SslMode::Require, Verify::Full)
    );
    assert_eq!(c.root_cert.as_deref(), Some("/tmp/ca.pem"));
    let c = parse("postgres://u@h/d?sslmode=require&sslrootcert=ca.pem").unwrap();
    assert_eq!(c.verify, Verify::Chain);
    let c = parse("host=localhost user=me sslmode=verify-ca dbname=x").unwrap();
    assert_eq!(
        (c.inner.get_ssl_mode(), c.verify),
        (SslMode::Require, Verify::Chain)
    );
    assert_eq!(c.inner.get_dbname(), Some("x"));
    let c = parse("postgres://u@h/d?sslmode=disable").unwrap();
    assert_eq!(c.inner.get_ssl_mode(), SslMode::Disable);
    for bad in [
        "postgres://u@h/d?sslmode=sometimes",
        "nonsense=1",
        "postgres://[bad",
    ] {
        let e = parse(bad).unwrap_err();
        assert_eq!(e.code, "EINVAL", "{bad}: {}", e.message);
    }
}

/// TLS is not exercised against a live server here (see docs/std/postgres.md): these only check that
/// every verification mode builds a rustls configuration with the runtime's provider.
#[test]
fn tls_configs_build_for_every_mode() {
    use super::tls::client_config;
    for verify in [Verify::None, Verify::Chain, Verify::Full] {
        assert!(client_config(verify, b"").is_ok(), "{verify:?}");
    }
    let bad = b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    assert!(client_config(Verify::Chain, bad).is_err());
    let config = parse("postgres://u@h/d?sslmode=verify-ca&sslrootcert=/no/such.pem").unwrap();
    let e = super::tls::connector(&config).err().unwrap();
    assert!(e.contains("sslrootcert"), "{e}");
}

/// `interval`'s binary form.
fn interval_raw(micros: i64, days: i32, months: i32) -> Vec<u8> {
    let mut raw = micros.to_be_bytes().to_vec();
    raw.extend(days.to_be_bytes());
    raw.extend(months.to_be_bytes());
    raw
}

#[test]
fn intervals_print_like_postgres() {
    // Expected strings are PostgreSQL 17's output for the same values.
    let iv = |micros: i64, days: i32, months: i32| interval(&interval_raw(micros, days, months));
    let hms = |h: i64, m: i64, s: i64| ((h * 60 + m) * 60 + s) * 1_000_000;
    assert_eq!(
        iv(hms(4, 5, 6) + 500_000, 3, 14).unwrap(),
        "1 year 2 mons 3 days 04:05:06.5"
    );
    assert_eq!(iv(-1_000_000, 0, 0).unwrap(), "-00:00:01");
    assert_eq!(iv(0, 0, 0).unwrap(), "00:00:00");
    assert_eq!(
        iv(hms(4, 0, 0), -3, -10).unwrap(),
        "-10 mons -3 days +04:00:00"
    );
    assert_eq!(iv(0, -1, 1).unwrap(), "1 mon -1 days");
    assert_eq!(iv(hms(100, 0, 0), 0, 0).unwrap(), "100:00:00");
    assert_eq!(iv(0, 0, -25).unwrap(), "-2 years -1 mons");
    assert_eq!(iv(1, 1, 0).unwrap(), "1 day 00:00:00.000001");
    assert!(interval(&[0; 15]).is_err());
    assert_eq!(json(&Type::INTERVAL, &interval_raw(0, 2, 0)), r#""2 days""#);
}

#[test]
fn money_and_network_types_print_like_postgres() {
    assert_eq!(money(&(-123_450i64).to_be_bytes()).unwrap(), "-1234.50");
    assert_eq!(money(&7i64.to_be_bytes()).unwrap(), "0.07");
    let v4 = |bits: u8, cidr: u8| inet(&[2, bits, cidr, 4, 10, 0, 0, 1]).unwrap();
    assert_eq!(v4(32, 0), "10.0.0.1");
    assert_eq!(v4(32, 1), "10.0.0.1/32");
    assert_eq!(v4(8, 0), "10.0.0.1/8");
    let mut v6 = vec![3, 128, 0, 16, 0x20, 0x01, 0x0d, 0xb8];
    v6.extend([0; 11]);
    v6.push(1);
    assert_eq!(inet(&v6).unwrap(), "2001:db8::1");
    let mut mapped = vec![3, 128, 0, 16];
    mapped.extend([0; 10]);
    mapped.extend([0xff, 0xff, 1, 2, 3, 4]);
    assert_eq!(inet(&mapped).unwrap(), "::ffff:1.2.3.4");
    assert!(inet(&[2, 32, 0, 4, 10]).is_err());
    assert_eq!(
        macaddr(&[8, 0, 0x2b, 1, 2, 3]).unwrap(),
        "08:00:2b:01:02:03"
    );
    assert_eq!(
        json(&Type::CIDR, &[2, 8, 1, 4, 10, 0, 0, 0]),
        r#""10.0.0.0/8""#
    );
}
