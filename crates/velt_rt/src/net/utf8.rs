//! Incremental UTF-8 decoding for `readString` on byte streams: a character split across two
//! reads is held back until its remaining bytes arrive; invalid bytes decode to U+FFFD.

/// Decoder state: the bytes of an incomplete trailing character from the previous chunk.
#[derive(Default, Debug)]
pub struct Utf8Decoder {
    tail: Vec<u8>,
}

impl Utf8Decoder {
    /// Decode `chunk` (after any held-back bytes). At `eof`, held-back bytes are flushed as U+FFFD.
    pub fn decode(&mut self, chunk: &[u8], eof: bool) -> String {
        let mut bytes = std::mem::take(&mut self.tail);
        bytes.extend_from_slice(chunk);
        let mut out = String::with_capacity(bytes.len());
        let mut rest = bytes.as_slice();
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    return out;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    // SAFETY: `from_utf8` validated this prefix.
                    out.push_str(unsafe { std::str::from_utf8_unchecked(valid) });
                    match e.error_len() {
                        Some(n) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            rest = &after[n..];
                        }
                        None if eof => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            return out;
                        }
                        None => {
                            self.tail = after.to_vec();
                            return out;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_characters_and_invalid_bytes() {
        let mut d = Utf8Decoder::default();
        let euro = "€".as_bytes(); // 3 bytes
        assert_eq!(d.decode(&[b'a', euro[0]], false), "a");
        assert_eq!(d.decode(&euro[1..2], false), "");
        assert_eq!(d.decode(&[euro[2], b'b'], false), "€b");
        assert_eq!(d.decode(&[0xff, b'c'], false), "\u{fffd}c");
        assert_eq!(d.decode(&[0xe2], false), "");
        assert_eq!(d.decode(&[], true), "\u{fffd}");
    }
}
