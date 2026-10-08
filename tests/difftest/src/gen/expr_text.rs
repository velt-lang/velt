//! String expressions, over ASCII and non-ASCII text alike: lengths and positions count UTF-16
//! code units in both languages (#377 phase 2b), so slicing may split a surrogate pair.

use super::scope::Ty;
use super::Gen;

const WORDS: [&str; 18] = [
    "",
    "a",
    "ab",
    "hello",
    "Hello World",
    "x-y-z",
    "  pad  ",
    "a,b,c",
    "zzz",
    "Q",
    "one two",
    "42",
    "héllo",
    "Zoë",
    "日本語",
    "😀",
    "a😀b,c",
    "～ é 🎉",
];

/// Fills for `padStart` / `padEnd`; a supplementary character can be cut between its halves.
const FILLS: [&str; 4] = ["*", "-=", "é", "😀"];

/// A generated expression plus whether it names a *place* (variable, field, element). Moving out
/// of a place is a Velt compile error (or a move), so moving positions copy places first.
pub struct Expr {
    /// Source text.
    pub text: String,
    /// True when `text` denotes an existing place rather than a fresh value.
    pub place: bool,
}

impl Expr {
    pub(super) fn fresh(text: String) -> Self {
        Expr { text, place: false }
    }
    pub(super) fn place(text: String) -> Self {
        Expr { text, place: true }
    }
}

impl Gen {
    /// A string literal from the word list.
    pub(super) fn string_lit(&mut self) -> String {
        format!("\"{}\"", self.rng.pick(&WORDS))
    }

    /// A `string` expression.
    pub(super) fn string(&mut self, d: u32) -> Expr {
        if d == 0 || self.rng.chance(25) {
            return self.string_atom();
        }
        let d = d - 1;
        match self.rng.below(12) {
            0..=1 => Expr::fresh(self.template(d)),
            2 => {
                let (a, b) = (self.string(d).text, self.string(d).text);
                Expr::fresh(format!("({a} + {b})"))
            }
            3 => {
                let m =
                    *self
                        .rng
                        .pick(&["toUpperCase", "toLowerCase", "trim", "trimStart", "trimEnd"]);
                Expr::fresh(format!("{}.{m}()", self.string(d).text))
            }
            4 => {
                let (s, a, b) = (
                    self.string(d).text,
                    self.rng.range(-6, 6),
                    self.rng.range(-6, 8),
                );
                let m = if a >= 0 && self.rng.chance(40) {
                    "substring"
                } else {
                    "slice"
                };
                Expr::fresh(format!("{s}.{m}({a}, {b})"))
            }
            5 => {
                let s = self.string(d).text;
                Expr::fresh(format!("{s}.repeat({})", self.rng.range(0, 3)))
            }
            6 => {
                let (s, m, n, f) = (
                    self.string(d).text,
                    self.rng.pick(&["padStart", "padEnd"]),
                    self.rng.range(0, 12),
                    self.rng.pick(&FILLS),
                );
                Expr::fresh(format!("{s}.{m}({n}, \"{f}\")"))
            }
            7 => {
                let (s, m) = (
                    self.string(d).text,
                    self.rng.pick(&["replace", "replaceAll"]),
                );
                let (a, b) = (self.string_lit(), self.string_lit());
                Expr::fresh(format!("{s}.{m}({a}, {b})"))
            }
            8 => {
                // Branches are copied: a ternary over places moves them in Velt (bug
                // `ternary-moves-place`), which would reject most programs.
                let (c, a, b) = (self.boolean(d), self.owned_string(d), self.owned_string(d));
                Expr::fresh(format!("({c} ? {a} : {b})"))
            }
            9 if self.rng.chance(40) => self.string_extra(d).unwrap_or_else(|| self.string_atom()),
            9 => self
                .call_expr(Ty::Str, d)
                .map_or_else(|| self.string_atom(), Expr::fresh),
            _ => self
                .string_from_collection(d)
                .unwrap_or_else(|| self.string_atom()),
        }
    }

    /// A string value that can be moved (stored, pushed, returned): places are copied through a
    /// template, which means the same thing in both languages.
    pub(super) fn owned_string(&mut self, d: u32) -> String {
        let e = self.string(d);
        if e.place {
            format!("`${{{}}}`", e.text)
        } else {
            e.text
        }
    }

    /// A template literal mixing every scalar type (floats print identically inside templates,
    /// including `-0` → `0`).
    pub(super) fn template(&mut self, d: u32) -> String {
        let mut t = String::from("`");
        for i in 0..self.rng.range(1, 3) {
            if i > 0 || self.rng.chance(50) {
                t.push_str(self.rng.pick(&["", "-", " ", "x=", ":"]));
            }
            let part = match self.rng.below(5) {
                0 => self.int(d),
                1 => self.float(d),
                2 => self.boolean(d),
                3 => self.union_text(),
                _ => self.string(d).text,
            };
            t.push_str(&format!("${{{part}}}"));
        }
        t.push('`');
        t
    }

    fn string_atom(&mut self) -> Expr {
        let vars = self.scope.of_type(Ty::Str);
        if !vars.is_empty() && self.rng.chance(60) {
            return Expr::place(self.rng.pick(&vars).name.clone());
        }
        Expr::fresh(self.string_lit())
    }

    /// A union variable, or a literal when none is in scope.
    fn union_text(&mut self) -> String {
        let vars = self.scope.of_type(Ty::Union);
        if vars.is_empty() {
            return "\"u\"".into();
        }
        self.rng.pick(&vars).name.clone()
    }
}
