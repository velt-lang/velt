//! Emulated string builder (rt_abi_async.md §12.1) and `velt_rt_str_eq`. A builder is an owned
//! `VeltStr`; every push reallocates (simple, and leak/double-free checked like any string).

use super::rt::{inspect_num, js_num};
use super::Interp;

/// `JSON.stringify` of a string: quotes and the escapes of §12.1.
pub(super) fn json_quote(s: &[u8]) -> Vec<u8> {
    let mut out = vec![b'"'];
    for &c in s {
        match c {
            b'"' => out.extend(b"\\\""),
            b'\\' => out.extend(b"\\\\"),
            b'\n' => out.extend(b"\\n"),
            b'\r' => out.extend(b"\\r"),
            b'\t' => out.extend(b"\\t"),
            8 => out.extend(b"\\b"),
            12 => out.extend(b"\\f"),
            c if c < 0x20 => out.extend(format!("\\u{c:04x}").bytes()),
            c => out.push(c),
        }
    }
    out.push(b'"');
    out
}

impl Interp<'_> {
    /// Append `more` to the builder at `b`.
    fn buf_push(&mut self, b: u64, more: &[u8]) {
        let mut bytes = self.str_bytes(b);
        bytes.extend_from_slice(more);
        let (p, cap) = (self.read_u64(b), self.read_u64(b + 16));
        if cap > 0 {
            self.heap_free(p);
        }
        self.new_str(b, &bytes);
    }

    /// String builder functions; `None` if `sym` is not one of them.
    pub(super) fn rt_strbuf(&mut self, sym: &str, a: &[u64]) -> Option<u64> {
        match sym {
            "velt_rt_strbuf_new" => self.write_bytes(a[1], &[0; 24]),
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
            "velt_rt_strbuf_push_i64" => self.buf_push(a[0], (a[1] as i64).to_string().as_bytes()),
            "velt_rt_strbuf_push_u64" => self.buf_push(a[0], a[1].to_string().as_bytes()),
            "velt_rt_strbuf_push_f64" => {
                let text = js_num(f64::from_bits(a[1]));
                self.buf_push(a[0], text.as_bytes());
            }
            "velt_rt_strbuf_push_inspect_f64" => {
                let text = inspect_num(f64::from_bits(a[1]));
                self.buf_push(a[0], text.as_bytes());
            }
            "velt_rt_strbuf_drop" => {
                let (p, _, cap) = self.str_header(a[0]);
                if cap > 0 {
                    self.heap_free(p);
                }
                self.write_bytes(a[0], &[0; 24]);
            }
            "velt_rt_strbuf_push_json_f64" => {
                let v = f64::from_bits(a[1]);
                let text = if v.is_finite() {
                    js_num(v)
                } else {
                    "null".into()
                };
                self.buf_push(a[0], text.as_bytes());
            }
            "velt_rt_strbuf_push_bool" => {
                let text: &[u8] = if a[1] & 1 == 1 { b"true" } else { b"false" };
                self.buf_push(a[0], text);
            }
            "velt_rt_strbuf_push_byte" => self.buf_push(a[0], &[a[1] as u8]),
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
            "velt_rt_strbuf_push_json_str" => {
                let s = self.str_bytes(a[1]);
                self.buf_push(a[0], &json_quote(&s));
            }
            "velt_rt_strbuf_finish" => {
                let hdr = self.read_bytes(a[0], 24);
                self.write_bytes(a[1], &hdr);
                self.write_bytes(a[0], &[0; 24]);
            }
            "velt_rt_str_eq" => return Some((self.str_bytes(a[0]) == self.str_bytes(a[1])) as u64),
            // Cycle tracking for printing (velt_rt's strbuf.rs): the programs run here print
            // no cyclic graphs, so every object is printed.
            "velt_rt_strbuf_inspect_enter" => return Some(1),
            "velt_rt_strbuf_inspect_begin" | "velt_rt_strbuf_inspect_leave" => {}
            // Only line breaking reads these, and the interpreter does not break lines.
            "velt_rt_strbuf_inspect_atom" => {}
            "velt_rt_strbuf_inspect_circular" => return Some(0),
            // Node's `maxArrayLength` (velt_rt's `inspect::push_more_items`).
            "velt_rt_strbuf_inspect_more" => {
                let s = if a[1] == 1 { "" } else { "s" };
                let text = format!(", ... {} more item{s}", a[1]);
                self.buf_push(a[0], text.as_bytes());
            }
            // Line breaking (velt_rt's inspect_layout): the programs run here print short values.
            "velt_rt_strbuf_len" => return Some(self.str_bytes(a[0]).len() as u64),
            "velt_rt_strbuf_inspect_layout" => {}
            _ => return None,
        }
        Some(0)
    }
}
