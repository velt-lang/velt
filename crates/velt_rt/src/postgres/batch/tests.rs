//! Batch replies decoded per mode, and the server's first error winning. (Column values are
//! covered by the `types` tests and the `postgres_batch` golden: tokio-postgres' `Column` has
//! no public constructor, so these use statements without result columns.)

use super::decode::results;
use super::Mode;
use bytes::BytesMut;

fn msg(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![tag];
    m.extend_from_slice(&(body.len() as u32 + 4).to_be_bytes());
    m.extend_from_slice(body);
    m
}

fn replies(parts: &[Vec<u8>]) -> BytesMut {
    let mut b = BytesMut::new();
    for p in parts {
        b.extend_from_slice(p);
    }
    b.extend_from_slice(&msg(b'Z', b"I"));
    b
}

fn complete(tag: &str) -> Vec<u8> {
    msg(b'C', format!("{tag}\0").as_bytes())
}

/// A row without columns.
fn row() -> Vec<u8> {
    msg(b'D', &[0, 0])
}

fn bound() -> Vec<u8> {
    msg(b'2', b"")
}

fn text(r: Result<Vec<u8>, crate::postgres::error::PgError>) -> String {
    String::from_utf8(r.unwrap()).unwrap()
}

#[test]
fn every_mode_has_one_entry_per_execution() {
    let r = || {
        replies(&[
            msg(b'1', b""),
            bound(),
            row(),
            row(),
            complete("SELECT 2"),
            bound(),
            complete("SELECT 0"),
            bound(),
            complete("UPDATE 7"),
        ])
    };
    assert_eq!(text(results(r(), &[], 3, Mode::Rows)), "[[{},{}],[],[]]");
    assert_eq!(text(results(r(), &[], 3, Mode::FirstRow)), "[{},null,null]");
    assert_eq!(text(results(r(), &[], 3, Mode::Count)), "[2,0,7]");
}

#[test]
fn the_first_error_fails_the_batch() {
    let error = msg(
        b'E',
        b"SERROR\0C23505\0Mduplicate key\0Dthe detail\0nitems_pkey\0\0",
    );
    let r = replies(&[bound(), complete("INSERT 0 1"), bound(), error]);
    let e = results(r, &[], 3, Mode::Count).unwrap_err();
    assert_eq!(e.code, "23505");
    assert_eq!(e.message, "duplicate key");
    assert_eq!(e.detail.as_deref(), Some("the detail"));
    assert_eq!(e.constraint.as_deref(), Some("items_pkey"));
}

#[test]
fn missing_results_are_reported() {
    let r = replies(&[bound(), complete("SELECT 0")]);
    let e = results(r, &[], 2, Mode::Rows).unwrap_err();
    assert!(e.message.contains("1 results for 2"), "{}", e.message);
}
