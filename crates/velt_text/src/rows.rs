//! The parts of a database driver's result rows that its row cursor shares with std's decoders
//! (native_abi.md "Rows", [`crate::rows_cursor`]): the header of column names and the tags a cell
//! crosses the C ABI with.
//!
//! Header: `single: u8, columns: u16, (name_len: u32, name bytes)*` (integers little-endian).
//! A name that is all ASCII says so ([`ASCII_NAME`] in its length), and a cell of text that is
//! all ASCII has the [`ASCII`] tag, because the driver looks at every byte anyway and the decoder
//! then need not: such a string's UTF-16 length is its byte length.

/// A cell's tags.
pub const NULL: u8 = 0;
pub const FALSE: u8 = 1;
pub const TRUE: u8 = 2;
pub const INT: u8 = 3;
pub const FLOAT: u8 = 4;
pub const TEXT: u8 = 5;
pub const BLOB: u8 = 6;
/// [`TEXT`] that is all ASCII.
pub const ASCII: u8 = 7;
/// A nested value (an array or object a database column holds, such as a Postgres array or
/// `jsonb`) as its JSON text, which the decoder reads as JSON.
pub const JSON: u8 = 8;
/// Set in a column name's length when the name is all ASCII.
pub const ASCII_NAME: u32 = 0x8000_0000;

/// The column-names part of a header (the count, then each name): what a cursor's header holds
/// after its `single` byte.
///
/// # Panics
/// With more than 65,535 columns or a name of 2 GiB or more (beyond what databases allow).
pub fn encode_names<'n, I>(columns: I) -> Vec<u8>
where
    I: IntoIterator<Item = &'n str>,
    I::IntoIter: ExactSizeIterator,
{
    let columns = columns.into_iter();
    let count = u16::try_from(columns.len()).expect("more database columns than 65,535");
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_le_bytes());
    for name in columns {
        assert!(
            name.len() < ASCII_NAME as usize,
            "database column name too long"
        );
        let ascii = if name.is_ascii() { ASCII_NAME } else { 0 };
        out.extend_from_slice(&(name.len() as u32 | ascii).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
    }
    out
}

/// Where the first column's name starts in the header (at its length prefix).
pub const NAMES: usize = 3;

/// The header `buf` starts with: whether it is a single row, the number of columns, and where
/// it ends. `None` unless every name is in bounds.
pub fn header_end(buf: &[u8]) -> Option<(bool, usize, usize)> {
    let single = *buf.first()? != 0;
    let count = u16::from_le_bytes(buf.get(1..3)?.try_into().ok()?) as usize;
    let mut pos = NAMES;
    for _ in 0..count {
        let len = u32::from_le_bytes(buf.get(pos..pos + 4)?.try_into().ok()?);
        pos += 4 + (len & !ASCII_NAME) as usize;
        if pos > buf.len() {
            return None;
        }
    }
    Some((single, count, pos))
}

/// The range of the column name whose length prefix is at `pos`, and whether the name is all
/// ASCII, in a buffer whose header [`header_end`] accepted; the next name's prefix is at the
/// range's end.
#[inline]
pub fn name_at(buf: &[u8], pos: usize) -> (usize, usize, bool) {
    let len = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
    let end = pos + 4 + (len & !ASCII_NAME) as usize;
    (pos + 4, end, len & ASCII_NAME != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_reads_back_its_names() {
        let mut buf = vec![1u8];
        buf.extend(encode_names(["id", "nöte"]));
        let (single, count, end) = header_end(&buf).unwrap();
        assert!(single);
        assert_eq!((count, end), (2, buf.len()));
        let (s, e, ascii) = name_at(&buf, NAMES);
        assert_eq!((&buf[s..e], ascii), (&b"id"[..], true));
        let (s, e, ascii) = name_at(&buf, e);
        assert_eq!((&buf[s..e], ascii), ("nöte".as_bytes(), false));
        // A header cut short is refused, not read out of bounds.
        assert_eq!(header_end(&buf[..buf.len() - 1]), None);
    }
}
