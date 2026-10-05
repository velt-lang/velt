//! Emulation of the `velt_rt` functions from rt_abi.md on the interpreter's memory. Heap strings
//! are tracked so tests can detect leaks, double frees and frees of static data.

use super::Interp;

/// JS `Number.prototype.toString` (enough for the tests: shortest round-trip, exponent form
/// outside `[1e-6, 1e21)`).
pub(super) fn js_num(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if v == 0.0 {
        return "0".into();
    }
    if !(1e-6..1e21).contains(&v.abs()) {
        let s = format!("{v:e}");
        let (m, e) = s.split_at(s.find('e').unwrap_or(s.len()));
        let e = &e[1..];
        return if e.starts_with('-') {
            format!("{m}e{e}")
        } else {
            format!("{m}e+{e}")
        };
    }
    format!("{v}")
}

/// `velt_rt_math_*` (round = JS `Math.round`: half toward +infinity).
fn math(sym: &str, x: f64) -> f64 {
    match sym {
        "velt_rt_math_sqrt" => x.sqrt(),
        "velt_rt_math_floor" => x.floor(),
        "velt_rt_math_ceil" => x.ceil(),
        "velt_rt_math_trunc" => x.trunc(),
        "velt_rt_math_fabs" => x.abs(),
        "velt_rt_math_round" => {
            let f = x.floor();
            if x - f >= 0.5 {
                f + 1.0
            } else {
                f
            }
        }
        other => panic!("interp: unknown extern {other}"),
    }
}

/// `velt_rt_pow_i64`: wrapping; negative exponent → 0 (1 if the base is 1).
fn pow_i64(b: i64, e: i64) -> i64 {
    match e {
        e if e < 0 => (b == 1) as i64,
        e => b.wrapping_pow(e.min(u32::MAX as i64) as u32),
    }
}

impl Interp<'_> {
    pub(super) fn str_header(&mut self, a: u64) -> (u64, u64, u64) {
        (
            self.read_u64(a),
            self.read_u64(a + 8),
            self.read_u64(a + 16),
        )
    }

    /// The bytes of the string at `a` (static or heap form: `w1` is `units << 32 | len`).
    pub(super) fn str_bytes(&mut self, a: u64) -> Vec<u8> {
        let (p, w1, _) = self.str_header(a);
        let len = w1 & 0xffff_ffff;
        if len == 0 {
            return vec![];
        }
        self.read_bytes(p, len as usize)
    }

    pub(super) fn str_text(&mut self, a: u64) -> String {
        String::from_utf8(self.str_bytes(a)).expect("interp: string is not UTF-8")
    }

    /// Write a new heap-owned string (`cap > 0`) to `out`.
    pub(super) fn new_str(&mut self, out: u64, bytes: &[u8]) {
        let p = self.heap_alloc(bytes.len() as u64);
        self.write_bytes(p, bytes);
        self.write_bytes(out, &p.to_le_bytes());
        let text = std::str::from_utf8(bytes).expect("interp: string is not UTF-8");
        let units = text.encode_utf16().count() as u64;
        let w1 = (units << 32) | bytes.len() as u64;
        self.write_bytes(out + 8, &w1.to_le_bytes());
        self.write_bytes(out + 16, &(bytes.len().max(1) as u64).to_le_bytes());
    }

    fn out(&mut self, stream: u64, s: &str) {
        match stream {
            1 => self.stdout.push_str(s),
            2 => self.stderr.push_str(s),
            _ => panic!("interp: bad stream {stream}"),
        }
    }

    /// Call runtime function `sym`; `Err(code)` means the process exited.
    pub(super) fn rt(&mut self, sym: &str, a: &[u64]) -> Result<u64, i32> {
        match sym {
            "velt_rt_str_concat" => {
                let mut b = self.str_bytes(a[0]);
                b.extend(self.str_bytes(a[1]));
                self.new_str(a[2], &b);
            }
            "velt_rt_str_from_i64" => self.new_str(a[1], (a[0] as i64).to_string().as_bytes()),
            "velt_rt_str_from_u64" => self.new_str(a[1], a[0].to_string().as_bytes()),
            "velt_rt_str_from_f64" => self.new_str(a[1], js_num(f64::from_bits(a[0])).as_bytes()),
            "velt_rt_str_from_bool" => {
                self.new_str(a[1], if a[0] & 1 == 1 { b"true" } else { b"false" })
            }
            "velt_rt_str_clone" | "velt_rt_str_own" => self.str_clone(a[0], a[1]),
            "velt_rt_str_drop" => {
                let (p, _, cap) = self.str_header(a[0]);
                if cap > 0 {
                    self.heap_free(p);
                }
                self.write_bytes(a[0], &[0; 24]);
            }
            "velt_rt_str_cmp" => {
                // Code-unit order, as the runtime's (#377 phase 2b).
                let (x, y) = (self.str_text(a[0]), self.str_text(a[1]));
                return Ok(x.encode_utf16().cmp(y.encode_utf16()) as i32 as u32 as u64);
            }
            "velt_rt_str_char_code_at" => {
                let units: Vec<u16> = self.str_text(a[0]).encode_utf16().collect();
                let unit = usize::try_from(a[1] as i64).ok().and_then(|i| units.get(i));
                return Ok(unit.map_or(-1, |&u| u as i64) as u64);
            }
            "velt_rt_write_str" => {
                let s = self.str_text(a[1]);
                self.out(a[0], &s);
            }
            "velt_rt_write_i64" => self.out(a[0], &(a[1] as i64).to_string()),
            "velt_rt_write_u64" => self.out(a[0], &a[1].to_string()),
            "velt_rt_write_f64" => self.out(a[0], &js_num(f64::from_bits(a[1]))),
            "velt_rt_write_bool" => self.out(a[0], if a[1] & 1 == 1 { "true" } else { "false" }),
            "velt_rt_write_byte" => self.out(a[0], &((a[1] as u8) as char).to_string()),
            "velt_rt_flush" => {}
            "velt_rt_panic" => {
                let msg = self.str_text(a[0]);
                self.stderr.push_str(&format!("panic: {msg}\n"));
                return Err(101);
            }
            "velt_rt_exit" => return Err(a[0] as u32 as i32),
            "velt_rt_set_throw_loc" => self.throw_loc = a[0],
            "velt_rt_throw_loc" => return Ok(self.throw_loc),
            "velt_rt_pow_f64" => {
                return Ok(f64::from_bits(a[0]).powf(f64::from_bits(a[1])).to_bits())
            }
            "velt_rt_pow_i64" => return Ok(pow_i64(a[0] as i64, a[1] as i64) as u64),
            "velt_rt_alloc" => {
                assert!(
                    a[1].is_power_of_two() && a[1] <= 8,
                    "interp: alignment {}",
                    a[1]
                );
                return Ok(self.heap_alloc(a[0]));
            }
            "velt_rt_realloc" => return Ok(self.realloc(a[0], a[1], a[3])),
            "velt_rt_free" => {
                assert_eq!(
                    self.allocs.get(&a[0]).copied(),
                    Some(a[1].max(1)),
                    "interp: free with wrong size"
                );
                self.heap_free(a[0]);
            }
            "velt_rt_str_hash" => {
                let b = self.str_bytes(a[0]);
                return Ok(b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| {
                    (h ^ x as u64).wrapping_mul(0x100_0000_01b3)
                }));
            }
            "velt_rt_rc_inc" => {
                let n = self.read_u64(a[0]) + 1;
                self.write_bytes(a[0], &n.to_le_bytes());
            }
            "velt_rt_rc_dec" => {
                let n = self.read_u64(a[0]) - 1;
                self.write_bytes(a[0], &n.to_le_bytes());
                return Ok((n == 0) as u64);
            }
            m if m.starts_with("velt_rt_math_") => {
                return Ok(math(m, f64::from_bits(a[0])).to_bits())
            }
            other => {
                return self
                    .rt_async(other, a)
                    .unwrap_or_else(|| panic!("interp: unknown extern {other}"))
            }
        }
        Ok(0)
    }

    fn realloc(&mut self, p: u64, old: u64, new: u64) -> u64 {
        assert_eq!(
            self.allocs.get(&p).copied(),
            Some(old.max(1)),
            "interp: realloc with wrong size"
        );
        let q = self.heap_alloc(new);
        let b = self.read_bytes(p, old.min(new) as usize);
        self.write_bytes(q, &b);
        self.heap_free(p);
        q
    }

    /// Deep copy; static strings (`cap == 0`) stay static.
    fn str_clone(&mut self, src: u64, dst: u64) {
        if self.str_header(src).2 == 0 {
            let b = self.read_bytes(src, 24);
            self.write_bytes(dst, &b);
        } else {
            let b = self.str_bytes(src);
            self.new_str(dst, &b);
        }
    }
}
