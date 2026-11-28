//! Emulated string builder (rt_abi_async.md §12.1). A builder is an owned
//! `VeltStr`; every push reallocates (simple, and leak/double-free checked like any string).

use super::rt::{inspect_num, js_num};
use super::Interp;

impl Interp<'_> {
    /// Append `more` to the builder at `b`.
    fn buf_push(&mut self, b: u64, more: &[u8]) {
        let mut bytes = self.str_bytes(b);
        bytes.extend_from_slice(more);
        if let Some(p) = self.str_heap(b) {
            self.heap_free(p);
        }
        self.new_str(b, &bytes);
    }

    /// String builder functions; `None` if `sym` is not one of them.
    pub(super) fn rt_strbuf(&mut self, sym: &str, a: &[u64]) -> Option<u64> {
        match sym {
            "velt_rt_strbuf_new" => self.write_bytes(a[1], &[0; 24]),
            // Never reuses the part (the runtime may always refuse; the template then starts a builder).
            "velt_rt_strbuf_adopt" => return Some(0),
            "velt_rt_strbuf_push_str" | "velt_rt_str_append" => {
                let s = self.str_bytes(a[1]);
                self.buf_push(a[0], &s);
            }
            "velt_rt_strbuf_push_bytes" => {
                // The low half of the length argument is the byte count.
                let len = a[2] & 0xffff_ffff;
                let s = if len == 0 {
                    vec![]
                } else {
                    self.read_bytes(a[1], len as usize)
                };
                self.buf_push(a[0], &s);
            }
            "velt_rt_strbuf_push_u8s" => {
                // The first `len` bytes of a `u8[]` (`{ ptr, len, cap }`): std's line breaking
                // builds its text this way.
                let len = a[2] as usize;
                let s = match len {
                    0 => vec![],
                    _ => {
                        let data = self.read_u64(a[1]);
                        self.read_bytes(data, len)
                    }
                };
                self.buf_push(a[0], &s);
            }
            "velt_test_strbuf_push_f64" => {
                let text = js_num(f64::from_bits(a[1]));
                self.buf_push(a[0], text.as_bytes());
            }
            "velt_test_strbuf_push_inspect_f64" => {
                let text = inspect_num(f64::from_bits(a[1]));
                self.buf_push(a[0], text.as_bytes());
            }
            "velt_rt_strbuf_drop" => {
                if let Some(p) = self.str_heap(a[0]) {
                    self.heap_free(p);
                }
                self.write_bytes(a[0], &[0; 24]);
            }
            "velt_rt_strbuf_push_bool" => {
                let text: &[u8] = if a[1] & 1 == 1 { b"true" } else { b"false" };
                self.buf_push(a[0], text);
            }
            "velt_rt_strbuf_push_byte" => self.buf_push(a[0], &[a[1] as u8]),
            // The first `n` bytes of three words, byte `i` in bits `8 * (i % 8)` of word `i / 8`
            // (std's `strbufPushAsciiWords` where the builder has no room inline).
            "velt_rt_strbuf_push_words" => {
                let words: Vec<u8> = a[1..4].iter().flat_map(|w| w.to_le_bytes()).collect();
                self.buf_push(a[0], &words[..a[4] as usize]);
            }
            "velt_rt_strbuf_push_inspect_str" => {
                let s = self.str_bytes(a[1]);
                let q = crate::lower::inspect_quote(&String::from_utf8_lossy(&s));
                self.buf_push(a[0], q.as_bytes());
            }
            "velt_rt_strbuf_push_inspect_key" => {
                let s = self.str_bytes(a[1]);
                let text = crate::lower::inspect_key(&String::from_utf8_lossy(&s));
                self.buf_push(a[0], text.as_bytes());
            }
            "velt_rt_strbuf_finish" => {
                let hdr = self.read_bytes(a[0], 24);
                self.write_bytes(a[1], &hdr);
                self.write_bytes(a[0], &[0; 24]);
            }
            // The test builder's `==` of strings (`tests/builder.rs` `str_key_hooks`).
            "velt_test_str_eq" => {
                return Some((self.str_bytes(a[0]) == self.str_bytes(a[1])) as u64)
            }
            _ => return None,
        }
        Some(0)
    }
}
