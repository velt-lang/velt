//! Integer, float and bool expressions.
//!
//! Integer expressions keep every value within ±16384 (wide literals up to 2^52 appear only as
//! operands of a reduced `+`/`-` or a division) (bitwise ops on values reduced `% 10007`)
//! and every intermediate below 2³¹: binary results are reduced `% 10007`, shifts only see operands
//! reduced `% 1000`. That keeps i64 wrapping and JS's 32-bit bitwise operators out of the picture.
//! Integer division is written `(Math.trunc(a / k) as i64)`, Velt's integer division for an
//! `i64` `a` (a `number` division, converted, when `a` is only literals), which truncates
//! identically in both languages (exact in JS doubles: every operand is below 2^53). Values the
//! JavaScript API returns as `number` (`indexOf`, `charCodeAt`, `length`) are converted with
//! `as i64`, which TypeScript erases. JS integers
//! are doubles, so `*`, `%`, `/` and negation can produce `-0`, which `console.log` prints as
//! `-0`; those results are normalized with `| 0` (identity on i64).

use super::scope::Ty;
use super::Gen;

/// Whether `e` names nothing but `true` and `false`: no variable, call or member.
fn literals_only(e: &str) -> bool {
    e.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
        .all(|w| w == "true" || w == "false")
}

/// `7` or `(-7)`.
fn is_literal(e: &str) -> bool {
    let e = e
        .strip_prefix("(-")
        .and_then(|e| e.strip_suffix(')'))
        .unwrap_or(e);
    !e.is_empty() && e.chars().all(|c| c.is_ascii_digit())
}

const FLOATS: [&str; 14] = [
    "0.5", "1.5", "2.25", "0.1", "0.2", "3.0", "100.0", "1e21", "1e-7", "123.456", "0.3", "7.0",
    "2.5", "1e300",
];

impl Gen {
    /// `e` reduced into ±10007, `-0` normalized to `0`; in wild mode `e` itself (values wrap).
    pub(super) fn reduce(&self, e: &str) -> String {
        if self.wild {
            format!("({e})")
        } else {
            format!("(({e}) % 10007 | 0)")
        }
    }

    /// An `i64` expression within ±10007 (after its own reduction).
    ///
    /// `number` is a JS number in Velt: a literal takes the `i64` it meets, but a compound
    /// expression of literals alone (`(~(true ? 3 : 4))`, `Math.trunc(7 / 2)`) is a `number`
    /// where nothing gives it a type (an operand of `~`, a union `i64 | string`). Such an
    /// expression is converted with `as i64`: exact (its value is a small integer), and
    /// TypeScript erases the `as`.
    pub(super) fn int(&mut self, d: u32) -> String {
        let e = self.int_expr(d);
        match !self.wild && literals_only(&e) && !is_literal(&e) {
            true => format!("({e} as i64)"),
            false => e,
        }
    }

    fn int_expr(&mut self, d: u32) -> String {
        if d == 0 || self.rng.chance(25) {
            return self.int_atom();
        }
        let d = d - 1;
        match self.rng.below(14) {
            0..=2 => {
                let op = *self.rng.pick(&["+", "-", "*"]);
                let (a, b) = (self.int(d), self.int(d));
                // A wide literal only meets `+`/`-` and is reduced at once: exact in JS doubles.
                let a = match op != "*" && self.rng.chance(20) {
                    true => self.wide_lit(),
                    false => a,
                };
                self.reduce(&format!("{a} {op} {b}"))
            }
            3 => format!("(({} % {}) | 0)", self.int(d), self.nonzero_lit()),
            4 if self.rng.chance(25) => {
                let (w, k) = (self.wide_lit(), self.nonzero_lit());
                self.reduce(&format!("(Math.trunc({w} / {k}) as i64)"))
            }
            4 => {
                let (a, k) = (self.int(d), self.nonzero_lit());
                format!("((Math.trunc({a} / {k}) as i64) | 0)")
            }
            5 => {
                let op = *self.rng.pick(&["&", "|", "^"]);
                format!("({} {op} {})", self.int(d), self.int(d))
            }
            6 if self.wild => {
                let op = *self.rng.pick(&["<<", ">>", ">>>", "/", "%"]);
                format!("({} {op} {})", self.int(d), self.int(d))
            }
            7 if self.wild && self.rng.chance(50) => self.int_cast(d),
            6 => format!("(({} % 1000) << {})", self.int(d), self.rng.range(0, 3)),
            7 => format!("({} >> {})", self.int(d), self.rng.range(0, 5)),
            8 if self.rng.chance(50) => format!("((-{}) | 0)", self.int(d)),
            8 => format!("(~{})", self.int(d)),
            9 => format!("({} ? {} : {})", self.boolean(d), self.int(d), self.int(d)),
            10 if self.rng.chance(50) => self.int_extra(d).unwrap_or_else(|| self.int_atom()),
            10 => self
                .call_expr(Ty::Int, d)
                .unwrap_or_else(|| self.int_atom()),
            11 => self.int_from_text(d),
            _ => self
                .int_from_collection(d)
                .unwrap_or_else(|| self.int_atom()),
        }
    }

    fn int_atom(&mut self) -> String {
        let vars = self.scope.of_type(Ty::Int);
        if !vars.is_empty() && self.rng.chance(60) {
            return self.rng.pick(&vars).name.clone();
        }
        if self.wild && self.rng.chance(40) {
            return self.edge_lit();
        }
        let n = self.rng.range(-20, 100);
        if n < 0 {
            format!("({n})")
        } else {
            n.to_string()
        }
    }

    /// Wild mode: an i64 edge value (written so that `i64::MIN` parses: `(-MAX - 1)`).
    fn edge_lit(&mut self) -> String {
        self.rng
            .pick(&[
                "9223372036854775807",
                "(-9223372036854775807 - 1)",
                "2147483647",
                "(-2147483648)",
                "4294967295",
                "(-1)",
                "255",
                "65536",
                "(-4611686018427387904)",
            ])
            .to_string()
    }

    /// Wild mode: numeric casts (Rust `as` semantics: truncation, sign/zero extension,
    /// saturating float→int).
    fn int_cast(&mut self, d: u32) -> String {
        match self.rng.below(5) {
            0 => format!("({} as i32 as i64)", self.int(d)),
            1 => format!("({} as u8 as i64)", self.int(d)),
            2 => format!("({} as u16 as i64)", self.int(d)),
            3 => format!("({} as u64 as i64)", self.int(d)),
            _ => format!("({} as i64)", self.float(d)),
        }
    }

    /// An integer literal beyond 32 bits (up to 2^52, so sums with small values stay exact in
    /// JS): catches i64 values truncated to 32 bits anywhere in the pipeline.
    fn wide_lit(&mut self) -> String {
        let magnitude = *self.rng.pick(&[
            2147483648i64,
            4294967296,
            4294967297,
            1099511627776,
            123456789012345,
            4503599627370495,
            9007199254740,
        ]);
        let n = magnitude + self.rng.range(-3, 3);
        if self.rng.chance(30) {
            format!("(-{n})")
        } else {
            n.to_string()
        }
    }

    /// A literal divisor: never zero (division by zero panics in Velt, yields Infinity in JS).
    fn nonzero_lit(&mut self) -> String {
        let k = self.rng.range(1, 13);
        if self.rng.chance(20) {
            format!("(-{k})")
        } else {
            k.to_string()
        }
    }

    fn int_from_text(&mut self, d: u32) -> String {
        let s = self.string(d).text;
        match self.rng.below(3) {
            0 => format!("({s}.length as i64)"),
            1 => format!("({s}.indexOf({}) as i64)", self.string_lit()),
            _ => format!("({s}.length > 0 ? ({s}.charCodeAt(0) as i64) : (-1))"),
        }
    }

    /// An `f64` expression (unbounded: NaN and Infinity are fair game).
    pub(super) fn float(&mut self, d: u32) -> String {
        if d == 0 || self.rng.chance(25) {
            return self.float_atom();
        }
        let d = d - 1;
        match self.rng.below(11) {
            0..=3 => {
                let op = *self.rng.pick(&["+", "-", "*", "/", "%"]);
                format!("({} {op} {})", self.float(d), self.float(d))
            }
            4 => {
                let f = *self
                    .rng
                    .pick(&["floor", "ceil", "round", "trunc", "abs", "sqrt", "sign"]);
                format!("Math.{f}({})", self.float(d))
            }
            5 if self.rng.chance(30) => self.hypot(d),
            5 => {
                let f = *self.rng.pick(&["min", "max"]);
                format!("Math.{f}({}, {})", self.float(d), self.float(d))
            }
            // Only powers with exact results: libm `pow` is 1 ulp off on macOS (bug
            // math-pow-platform-rounding) and V8's isn't correctly rounded either.
            6 if self.rng.chance(70) => format!(
                "Math.pow(({} as f64), {})",
                self.int(d),
                self.rng.pick(&["2.0", "3.0", "1.0", "0.0"])
            ),
            6 => format!(
                "Math.pow({}, {})",
                self.float(d),
                self.rng.pick(&["1.0", "0.0"])
            ),
            7 => format!("({} as f64)", self.int(d)),
            8 => format!(
                "({} ? {} : {})",
                self.boolean(d),
                self.float(d),
                self.float(d)
            ),
            9 if self.rng.chance(50) => self.float_extra(d),
            9 => format!("parseFloat(`${{{}}}`)", self.float(d)),
            _ => self
                .call_expr(Ty::Float, d)
                .unwrap_or_else(|| self.float_atom()),
        }
    }

    fn float_atom(&mut self) -> String {
        let vars = self.scope.of_type(Ty::Float);
        if !vars.is_empty() && self.rng.chance(60) {
            return self.rng.pick(&vars).name.clone();
        }
        let lit = *self.rng.pick(&FLOATS);
        if self.rng.chance(20) {
            format!("(-{lit})")
        } else {
            lit.to_string()
        }
    }

    /// A `bool` expression.
    pub(super) fn boolean(&mut self, d: u32) -> String {
        if d == 0 || self.rng.chance(20) {
            return self.bool_atom();
        }
        let d = d - 1;
        let cmp = *self.rng.pick(&["<", "<=", ">", ">=", "==", "!="]);
        match self.rng.below(10) {
            0..=1 => format!("({} {cmp} {})", self.int(d), self.int(d)),
            2 => format!("({} {cmp} {})", self.float(d), self.float(d)),
            3 => format!("({} {cmp} {})", self.string(d).text, self.string(d).text),
            4 => format!("(!{})", self.boolean(d)),
            5 => {
                let op = *self.rng.pick(&["&&", "||"]);
                format!("({} {op} {})", self.boolean(d), self.boolean(d))
            }
            6 => {
                let m = *self.rng.pick(&["includes", "startsWith", "endsWith"]);
                format!("{}.{m}({})", self.string(d).text, self.string_lit())
            }
            7 if self.rng.chance(40) => self.bool_from_tagged().unwrap_or_else(|| self.bool_atom()),
            7 => self
                .call_expr(Ty::Bool, d)
                .unwrap_or_else(|| self.bool_atom()),
            _ => self
                .bool_from_collection(d)
                .unwrap_or_else(|| self.bool_atom()),
        }
    }

    fn bool_atom(&mut self) -> String {
        let vars = self.scope.of_type(Ty::Bool);
        if !vars.is_empty() && self.rng.chance(60) {
            return self.rng.pick(&vars).name.clone();
        }
        self.rng.pick(&["true", "false"]).to_string()
    }
}
