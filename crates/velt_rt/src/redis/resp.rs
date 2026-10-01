//! RESP2, the Redis wire protocol: commands are encoded as arrays of bulk strings, replies are
//! parsed incrementally.
//!
//! The parser keeps a stack of partially received arrays, so a large reply arriving in many
//! chunks is parsed once, in linear time: each call consumes only complete frames (a scalar or an
//! array header) and leaves an incomplete frame's bytes in the caller's buffer.

/// One reply value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `$-1` / `*-1`: a missing value.
    Nil,
    /// `+OK`: a simple status string.
    Status(String),
    /// `-ERR …`: an error reply (the whole line, code first).
    Error(String),
    /// `:42`.
    Int(i64),
    /// `$n`: a bulk string (binary safe).
    Bulk(Vec<u8>),
    /// `*n`: nested values.
    Array(Vec<Value>),
}

/// Bulk strings and arrays larger than Redis' own 512 MiB limit are protocol errors.
const MAX_LEN: i64 = 512 * 1024 * 1024;

/// Append the RESP encoding of one command (`args[0]` is the command name) to `out`.
pub fn encode_command<A: AsRef<[u8]>>(args: &[A], out: &mut Vec<u8>) {
    out.push(b'*');
    push_int(out, args.len() as i64);
    for arg in args {
        let arg = arg.as_ref();
        out.push(b'$');
        push_int(out, arg.len() as i64);
        out.extend_from_slice(arg);
        out.extend_from_slice(b"\r\n");
    }
}

fn push_int(out: &mut Vec<u8>, n: i64) {
    out.extend_from_slice(itoa::Buffer::new().format(n).as_bytes());
    out.extend_from_slice(b"\r\n");
}

/// One frame at the start of a buffer.
enum Frame {
    /// A complete scalar value.
    Scalar(Value),
    /// The header of an array with this many elements (> 0).
    ArrayHeader(usize),
}

/// Incremental reply parser.
#[derive(Default)]
pub struct Parser {
    /// Arrays still receiving elements: (expected length, elements so far).
    stack: Vec<(usize, Vec<Value>)>,
}

impl Parser {
    /// Parse the complete replies at the start of `buf`, appending them to `out`. Returns how
    /// many bytes were consumed; the caller keeps the rest and calls again with more data.
    pub fn feed(&mut self, buf: &[u8], out: &mut Vec<Value>) -> Result<usize, String> {
        let mut pos = 0;
        while let Some((frame, used)) = frame(&buf[pos..])? {
            pos += used;
            match frame {
                Frame::ArrayHeader(n) => self.stack.push((n, Vec::with_capacity(n.min(4096)))),
                Frame::Scalar(v) => self.complete(v, out),
            }
        }
        Ok(pos)
    }

    /// Hand a finished value to its parent array (completing parents in turn) or to `out`.
    fn complete(&mut self, mut v: Value, out: &mut Vec<Value>) {
        while let Some((want, items)) = self.stack.last_mut() {
            items.push(v);
            if items.len() < *want {
                return;
            }
            let (_, items) = self.stack.pop().expect("ICE: non-empty parser stack");
            v = Value::Array(items);
        }
        out.push(v);
    }
}

/// The frame at the start of `buf`, with its length in bytes; `None` if it is incomplete.
fn frame(buf: &[u8]) -> Result<Option<(Frame, usize)>, String> {
    let Some(end) = buf.windows(2).position(|w| w == b"\r\n") else {
        return Ok(None);
    };
    let Some((&tag, line)) = buf[..end].split_first() else {
        return Err("empty reply line".to_string());
    };
    let after = end + 2;
    let text = || String::from_utf8_lossy(line).into_owned();
    let v = match tag {
        b'+' => Value::Status(text()),
        b'-' => Value::Error(text()),
        b':' => Value::Int(number(line)?),
        b'$' => return bulk(buf, number(line)?, after),
        b'*' => match number(line)? {
            n if n < 0 => Value::Nil,
            0 => Value::Array(vec![]),
            n if n > MAX_LEN => return Err(format!("array too long ({n})")),
            n => return Ok(Some((Frame::ArrayHeader(n as usize), after))),
        },
        other => return Err(format!("unexpected reply type byte 0x{other:02x}")),
    };
    Ok(Some((Frame::Scalar(v), after)))
}

fn bulk(buf: &[u8], len: i64, start: usize) -> Result<Option<(Frame, usize)>, String> {
    if len < 0 {
        return Ok(Some((Frame::Scalar(Value::Nil), start)));
    }
    if len > MAX_LEN {
        return Err(format!("bulk string too long ({len})"));
    }
    let end = start + len as usize;
    if buf.len() < end + 2 {
        return Ok(None);
    }
    if &buf[end..end + 2] != b"\r\n" {
        return Err("bulk string not terminated by CRLF".to_string());
    }
    let v = Value::Bulk(buf[start..end].to_vec());
    Ok(Some((Frame::Scalar(v), end + 2)))
}

fn number(line: &[u8]) -> Result<i64, String> {
    std::str::from_utf8(line)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("invalid number {:?}", String::from_utf8_lossy(line)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(chunks: &[&[u8]]) -> Vec<Value> {
        let (mut p, mut buf, mut out) = (Parser::default(), Vec::new(), Vec::new());
        for c in chunks {
            buf.extend_from_slice(c);
            let used = p.feed(&buf, &mut out).unwrap();
            buf.drain(..used);
        }
        assert!(buf.is_empty(), "unconsumed: {buf:?}");
        out
    }

    #[test]
    fn encodes_commands() {
        let mut out = vec![];
        encode_command(&["SET", "k", "héllo"], &mut out);
        assert_eq!(out, b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$6\r\nh\xc3\xa9llo\r\n");
    }

    #[test]
    fn parses_every_type() {
        let v = parse_all(&[b"+OK\r\n-ERR bad\r\n:-7\r\n$3\r\nabc\r\n$-1\r\n*-1\r\n*0\r\n"]);
        assert_eq!(
            v,
            vec![
                Value::Status("OK".into()),
                Value::Error("ERR bad".into()),
                Value::Int(-7),
                Value::Bulk(b"abc".to_vec()),
                Value::Nil,
                Value::Nil,
                Value::Array(vec![]),
            ]
        );
    }

    #[test]
    fn parses_nested_arrays_split_anywhere() {
        let wire: &[u8] = b"*2\r\n*2\r\n:1\r\n$2\r\nab\r\n$0\r\n\r\n:5\r\n";
        let want = vec![
            Value::Array(vec![
                Value::Array(vec![Value::Int(1), Value::Bulk(b"ab".to_vec())]),
                Value::Bulk(vec![]),
            ]),
            Value::Int(5),
        ];
        for split in 0..wire.len() {
            assert_eq!(parse_all(&[&wire[..split], &wire[split..]]), want);
        }
        let bytes: Vec<&[u8]> = wire.chunks(1).collect();
        assert_eq!(parse_all(&bytes), want);
    }

    #[test]
    fn rejects_garbage() {
        let mut out = vec![];
        assert!(Parser::default().feed(b"?x\r\n", &mut out).is_err());
        assert!(Parser::default().feed(b":x\r\n", &mut out).is_err());
        assert!(Parser::default().feed(b"$1\r\nab\r\n", &mut out).is_err());
    }
}
