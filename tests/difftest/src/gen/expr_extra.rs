//! Expressions over the less common features: nullable ints, object literals (records), class
//! statics and accessors, the generated generic/recursive library functions, `parseInt` /
//! `Number`, `Math` constants, nested templates and more array searches.

use super::expr_text::Expr;
use super::scope::Ty;
use super::Gen;

/// Inputs for `parseInt` / `Number` / `parseFloat` that exercise their parsing rules.
const NUMERIC_TEXT: [&str; 10] = [
    "42abc", "  -7", "ff", "0x1A", "", "3.9", "1e3", "-0", "12px", "Infinity",
];

impl Gen {
    /// An `i64` from the extra features, if one applies in the current scope.
    pub(super) fn int_extra(&mut self, d: u32) -> Option<String> {
        match self.rng.below(10) {
            8..=9 => self.int_from_tagged(d),
            0 => {
                let n = self.pick_var(Ty::OptInt)?;
                Some(if self.scope.closures == 0 && self.rng.chance(50) {
                    format!("({n} != null ? {n} : {})", self.int(d))
                } else {
                    format!("({n} ?? {})", self.int(d))
                })
            }
            1 => Some("K0.LIMIT".into()),
            2 => {
                let o = self.pick_object()?;
                Some(format!("{o}.twice"))
            }
            3 => {
                let r = self.pick_var(Ty::Rec)?;
                Some(format!("{r}.x"))
            }
            4 => Some(format!("maxOf({}, {})", self.int(d), self.int(d))),
            5 => Some(format!("rec({} & 15, {})", self.int(d), self.int(d))),
            6 => {
                let xs = self.array(Ty::IntArr, d).text;
                Some(format!("countOf({xs}, {})", self.int(d)))
            }
            _ => {
                let xs = self.array(Ty::IntArr, d).text;
                if self.rng.chance(50) {
                    return Some(format!("({xs}.lastIndexOf({}) as i64)", self.int(d)));
                }
                let pred = self.callback(&xs, Ty::IntArr, |g, d| g.boolean(d), d);
                Some(format!("({xs}.find({pred}) ?? {})", self.int(d)))
            }
        }
    }

    /// An `f64` from the extra features.
    pub(super) fn float_extra(&mut self, d: u32) -> String {
        let text = *self.rng.pick(&NUMERIC_TEXT);
        match self.rng.below(5) {
            0 => self.rng.pick(&["Math.PI", "Math.E"]).to_string(),
            1 => {
                let radix = self.rng.pick(&["", ", 10", ", 16", ", 2", ", 36"]);
                format!("parseInt(\"{text}\"{radix})")
            }
            2 => format!("Number(\"{text}\")"),
            3 => format!("parseFloat(\"{text}\")"),
            // `maxOf` on floats would keep hitting bug generic-compare-nan (NaN operands).
            _ => format!("Math.max({}, {})", self.float(d), self.float(d)),
        }
    }

    /// `Math.hypot` where the result is exact, so V8 (not correctly rounded) and Velt must agree:
    /// Pythagorean triples scaled by powers of two up to the overflow/underflow edges (Velt
    /// scales operands to avoid both), one zero operand, or an infinite / NaN operand.
    pub(super) fn hypot(&mut self, d: u32) -> String {
        match self.rng.below(5) {
            0..=2 => {
                let (a, b) = *self
                    .rng
                    .pick(&[(3.0, 4.0), (0.75, 1.0), (6.0, 8.0), (1.5, 2.0)]);
                let scale = 2f64.powi(self.rng.range(-1020, 1020) as i32);
                let sign = |g: &mut Gen| if g.rng.chance(30) { "-" } else { "" };
                let (sa, sb) = (sign(self), sign(self));
                let (a, b) = (
                    format!("({sa}{:?})", a * scale),
                    format!("({sb}{:?})", b * scale),
                );
                match self.rng.chance(50) {
                    true => format!("Math.hypot({a}, {b})"),
                    false => format!("Math.hypot({b}, {a})"),
                }
            }
            3 => format!("Math.hypot({}, 0.0)", self.float(d)),
            _ => {
                let special = *self
                    .rng
                    .pick(&["(1.0 / 0.0)", "(-1.0 / 0.0)", "(0.0 / 0.0)"]);
                format!("Math.hypot({}, {special})", self.float(d))
            }
        }
    }

    /// A `string` from the extra features.
    pub(super) fn string_extra(&mut self, d: u32) -> Option<Expr> {
        match self.rng.below(5) {
            3..=4 => self.string_from_tagged(d),
            0 => {
                let (a, b) = (self.owned_string(d), self.owned_string(d));
                Some(Expr::fresh(format!("maxOf({a}, {b})")))
            }
            1 => {
                let r = self.pick_var(Ty::Rec)?;
                Some(Expr::place(format!("{r}.s")))
            }
            _ => {
                let inner = self.template(d);
                Some(Expr::fresh(format!("`<${{{inner}}}>`")))
            }
        }
    }

    /// `{ x: <i64>, s: <string> }` (an anonymous struct in Velt), sometimes spreading a record
    /// local with `s` replaced (spreading a local consumes its non-Copy fields in Velt unless
    /// they are overridden, so JS aliasing never shows).
    pub(super) fn record_literal(&mut self, d: u32) -> String {
        if let Some(r) = self.pick_var(Ty::Rec).filter(|_| self.rng.chance(40)) {
            let s = self.owned_string(d);
            return match self.rng.chance(50) {
                true => format!("{{ ...{r}, s: {s} }}"),
                false => format!("{{ ...{r}, x: {}, s: {s} }}", self.int(d)),
            };
        }
        // `as i64`: an unannotated literal's field would be a `number` (JS's type for `x: 7`).
        let (x, s) = (self.int(d), self.owned_string(d));
        format!("{{ x: ({x} as i64), s: {s} }}")
    }

    /// A nullable int value: `null` or an int.
    pub(super) fn opt_int(&mut self, d: u32) -> String {
        if self.rng.chance(30) {
            "null".into()
        } else {
            self.int(d)
        }
    }

    /// A visible class instance whose accessors may be used.
    fn pick_object(&mut self) -> Option<String> {
        let objs = self.scope.objects();
        (!objs.is_empty()).then(|| self.rng.pick(&objs).name.clone())
    }

    /// One compound assignment to an `i64` binding, re-reduced on the same line.
    pub(super) fn compound_int(&mut self, name: &str) -> String {
        let (op, rhs) = match self.rng.below(4) {
            0 => ("<<=", self.rng.range(0, 3).to_string()),
            1 => (">>=", self.rng.range(0, 5).to_string()),
            _ => (
                *self.rng.pick(&["+=", "-=", "*=", "&=", "|=", "^="]),
                self.int(2),
            ),
        };
        format!("{name} {op} {rhs}; {name} = {};", self.reduce(name))
    }
}
