//! A row cursor and a parameter sink for velt/db's native API (crates/veltc/tests/native_rows.rs):
//! `n` rows of `id` (an integer), `name` (text), `score` (a float) and `note` (text, null in odd
//! rows), a cursor of the other cell kinds (`velt_rows__mixed`), and a sink that writes down what
//! it was handed.

use velt_native::export;
use velt_text::rows::encode_names;
use velt_text::rows_cursor::{close_cursor, close_sink, open_cursor, open_sink, Cell, ParamSink, RowCursor};

velt_native::package!(rows);

struct Rows {
    header: Vec<u8>,
    n: usize,
    row: usize,
    name: String,
}

impl RowCursor for Rows {
    fn header(&self) -> &[u8] {
        &self.header
    }
    fn step(&mut self) -> bool {
        if self.row == self.n {
            return false;
        }
        self.row += 1;
        self.name = format!("row {}", self.row);
        true
    }
    fn cell(&self, col: usize) -> Cell<'_> {
        match col {
            0 => Cell::Int(self.row as i64),
            1 => Cell::Text(self.name.as_bytes()),
            2 => Cell::Float(self.row as f64 * 1.5),
            _ if self.row % 2 == 1 => Cell::Null,
            _ => Cell::Ascii(b"even"),
        }
    }
}

/// Rows of the other cell kinds: `ok` (a boolean), `data` (a blob), `meta` (a nested value, as
/// its JSON text), `raw` (text that is not UTF-8), `whole` (a float that is an integer) and
/// `maybe` (null in odd rows; else ASCII text, or in every third even row non-ASCII text that
/// needs escapes, short, or long in rows 6, 12, ...: over 64 UTF-16 units).
struct Mixed {
    header: Vec<u8>,
    n: usize,
    row: usize,
    meta: String,
    long: Vec<u8>,
}

/// Short non-ASCII text with characters JSON escapes.
const SHORT: &str = "s\"é\t😀";

impl RowCursor for Mixed {
    fn header(&self) -> &[u8] {
        &self.header
    }
    fn step(&mut self) -> bool {
        if self.row == self.n {
            return false;
        }
        self.row += 1;
        self.meta = format!(
            "{{\"tags\": [\"t{}\", \"x\"], \"n\": {}, \"deep\": {{\"k\": [true, null]}}}}",
            self.row, self.row
        );
        true
    }
    fn cell(&self, col: usize) -> Cell<'_> {
        match col {
            0 => Cell::Bool(self.row % 2 == 0),
            1 => Cell::Blob(&[0, 7, 255][..self.row.min(3)]),
            2 => Cell::Json(self.meta.as_bytes()),
            3 => Cell::Text(b"caf\xe9 \"q\""),
            4 => Cell::Float(self.row as f64 * 2.0),
            _ if self.row % 2 == 1 => Cell::Null,
            _ if self.row % 6 == 4 => Cell::Text(SHORT.as_bytes()),
            _ if self.row % 6 == 0 => Cell::Text(&self.long),
            _ => Cell::Ascii(b"set"),
        }
    }
}

#[derive(Default)]
struct Taken(Vec<String>);

impl ParamSink for Taken {
    fn object(&mut self) {
        self.0.push("object".into());
    }
    fn name(&mut self, name: &[u8]) {
        self.0.push(format!("{}=", String::from_utf8_lossy(name)));
    }
    fn value(&mut self, v: Cell<'_>) {
        let text = match v {
            Cell::Null => "null".into(),
            Cell::Bool(b) => format!("{b}"),
            Cell::Int(i) => format!("int {i}"),
            Cell::Float(f) => format!("float {f}"),
            Cell::Text(t) | Cell::Ascii(t) => format!("text {}", String::from_utf8_lossy(t)),
            Cell::Blob(b) => format!("blob {b:?}"),
            Cell::Json(j) => format!("json {}", String::from_utf8_lossy(j)),
        };
        self.0.push(text);
    }
}

/// A cursor over `n` rows (`single`: a cursor for one row, as a driver opens for `get`),
/// registered with the runtime: the handle is a key velt/db looks up, not an address.
#[export]
fn velt_rows__cursor(n: u32, single: bool) -> u64 {
    let mut header = vec![single as u8];
    header.extend(encode_names(["id", "name", "score", "note"]));
    open_cursor(Box::new(Rows {
        header,
        n: n as usize,
        row: 0,
        name: String::new(),
    }))
}

/// A cursor over `n` rows of the other cell kinds ([`Mixed`]).
#[export]
fn velt_rows__mixed(n: u32, single: bool) -> u64 {
    let mut header = vec![single as u8];
    header.extend(encode_names(["ok", "data", "meta", "raw", "whole", "maybe"]));
    open_cursor(Box::new(Mixed {
        header,
        n: n as usize,
        row: 0,
        meta: String::new(),
        long: format!("{}\"\n", "é".repeat(66)).into_bytes(),
    }))
}

/// Gives a mixed cursor back: the number of rows it stepped over (0 for a handle that is not an
/// open mixed cursor).
#[export]
fn velt_rows__close_mixed(cursor: u64) -> u32 {
    close_cursor::<Mixed>(cursor).map_or(0, |rows| rows.row as u32)
}

/// Gives a cursor back: the number of rows it stepped over (0 for a handle that is not an open
/// cursor of this library).
#[export]
fn velt_rows__close(cursor: u64) -> u32 {
    close_cursor::<Rows>(cursor).map_or(0, |rows| rows.row as u32)
}

/// A parameter sink.
#[export]
fn velt_rows__sink() -> u64 {
    open_sink(Box::<Taken>::default())
}

/// Gives a sink back: what it was handed, one item per line ("" for a handle that is not an
/// open sink).
#[export]
fn velt_rows__taken(sink: u64) -> String {
    close_sink::<Taken>(sink).map_or(String::new(), |taken| taken.0.join(" "))
}
