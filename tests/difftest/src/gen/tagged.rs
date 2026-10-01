//! Tagged data: the discriminated union `Sh`, the literal union `Lvl`, the string enum `Col` and
//! the numeric enum `Num` — their declarations, values and the expressions that read them
//! (discriminant tests, narrowing ternaries, enum values). `switch` over them is in `switch.rs`.
//!
//! Object literals of `Sh` are only generated where `Sh` is the expected type (annotated
//! declarations, assignments, arguments), which is where Velt picks the union member; their
//! fields are written in declaration order so `JSON.stringify` agrees with JS's insertion order.

use super::expr_text::Expr;
use super::scope::{Ty, Var};
use super::Gen;

/// The members of `Sh`: discriminant and the fields after it (all members have `kind`; `pt` and
/// `box` share `x`, only `box` has `s`).
pub(super) const SHAPE_KINDS: [&str; 3] = ["pt", "box", "nil"];

/// The literals of `Lvl`.
pub(super) const LEVELS: [&str; 3] = ["lo", "mid", "hi"];

/// The members of `Col`.
pub(super) const COLORS: [&str; 3] = ["R", "G", "B"];

/// Whether member `kind` of `Sh` has field `x`.
pub(super) fn has_x(kind: &str) -> bool {
    kind != "nil"
}

impl Gen {
    /// Visible bindings of `ty` that no enclosing `case` has narrowed (tag tests on those could
    /// be impossible, which is an error).
    pub(super) fn unnarrowed(&self, ty: Ty) -> Vec<Var> {
        let vars = self.scope.of_type(ty).into_iter();
        vars.filter(|v| !v.narrowed).collect()
    }

    fn unnarrowed_shape(&mut self) -> Option<String> {
        let vars = self.unnarrowed(Ty::Shape);
        (!vars.is_empty()).then(|| self.rng.pick(&vars).name.clone())
    }

    /// `type Sh`, `type Lvl`, `enum Col` (string values vary per seed) and `enum Num`.
    pub(super) fn tagged_decl(&mut self) {
        let words = ["red", "green", "blue", "Red", "", "x y", "blue2"];
        let mut values: Vec<&str> = Vec::new();
        while values.len() < 3 {
            let w = *self.rng.pick(&words);
            if !values.contains(&w) {
                values.push(w);
            }
        }
        let (b, c) = (self.rng.range(0, 3), self.rng.range(-2, 9));
        self.out.push_str(concat!(
            "\ntype Sh = { kind: \"pt\"; x: i64 } | { kind: \"box\"; x: i64; s: string }",
            " | { kind: \"nil\" };\n",
            "type Lvl = \"lo\" | \"mid\" | \"hi\";\n",
        ));
        self.out.push_str(&format!(
            "enum Col {{ R = \"{}\", G = \"{}\", B = \"{}\" }}\n",
            values[0], values[1], values[2]
        ));
        let second = if b > 0 {
            format!("B = {}", b + 1)
        } else {
            "B".into()
        };
        let third = if c > b + 1 {
            format!("C = {c}")
        } else {
            "C".into()
        };
        self.out
            .push_str(&format!("enum Num {{ A, {second}, {third} }}\n"));
    }

    /// An object literal of a random `Sh` member (only where `Sh` is the expected type).
    pub(super) fn shape_literal(&mut self, d: u32) -> String {
        match *self.rng.pick(&SHAPE_KINDS) {
            "pt" => format!("{{ kind: \"pt\", x: {} }}", self.int(d)),
            "box" => {
                let (x, s) = (self.int(d), self.owned_string(d));
                format!("{{ kind: \"box\", x: {x}, s: {s} }}")
            }
            _ => "{ kind: \"nil\" }".into(),
        }
    }

    /// A value of the Copy tagged types: a variable or a literal / enum member.
    pub(super) fn tag_value(&mut self, ty: Ty) -> String {
        if let Some(v) = self.pick_var(ty).filter(|_| self.rng.chance(50)) {
            return v;
        }
        match ty {
            Ty::Lvl => format!("\"{}\"", self.rng.pick(&LEVELS)),
            _ => format!("Col.{}", self.rng.pick(&COLORS)),
        }
    }

    /// An `i64` read from tagged data, if the scope has some.
    pub(super) fn int_from_tagged(&mut self, d: u32) -> Option<String> {
        match self.rng.below(4) {
            0 => Some(format!("(Num.{} as i64)", self.rng.pick(&["A", "B", "C"]))),
            1 => {
                // Ternary narrowing of a local: not inside closures (captured unions).
                let v = self
                    .unnarrowed_shape()
                    .filter(|_| self.scope.closures == 0)?;
                let k = *self.rng.pick(&["pt", "box"]);
                let (op, fallback) = (*self.rng.pick(&["===", "=="]), self.int(d));
                Some(format!("({v}.kind {op} \"{k}\" ? {v}.x : {fallback})"))
            }
            // String members of literal unions need a detour through `+` / a template: bug
            // literal-union-methods.
            2 => {
                let v = self.pick_var(Ty::Shape)?;
                Some(format!("(`${{{v}.kind}}`.length as i64)"))
            }
            _ => {
                let l = self.tag_value(Ty::Lvl);
                Some(format!("(({l} + \"\").length as i64)"))
            }
        }
    }

    /// A `string` read from tagged data.
    pub(super) fn string_from_tagged(&mut self, d: u32) -> Option<Expr> {
        match self.rng.below(4) {
            0 => {
                let v = self.pick_var(Ty::Shape)?;
                Some(Expr::fresh(format!("`${{{v}.kind}}`")))
            }
            1 => {
                let v = self
                    .unnarrowed_shape()
                    .filter(|_| self.scope.closures == 0)?;
                let fallback = self.owned_string(d);
                Some(Expr::fresh(format!(
                    "({v}.kind != \"box\" ? {fallback} : `${{{v}.s}}!`)"
                )))
            }
            2 => {
                let c = self.tag_value(Ty::Col);
                Some(Expr::fresh(format!("`${{{c}}}`")))
            }
            _ => {
                let l = self.tag_value(Ty::Lvl);
                let text = format!("`<${{{l}}}>`");
                Some(Expr::fresh(text))
            }
        }
    }

    /// A `bool` testing tagged data.
    pub(super) fn bool_from_tagged(&mut self) -> Option<String> {
        let op = *self.rng.pick(&["===", "!==", "==", "!="]);
        match self.rng.below(3) {
            0 => {
                let v = self.unnarrowed_shape()?;
                Some(format!(
                    "({v}.kind {op} \"{}\")",
                    self.rng.pick(&SHAPE_KINDS)
                ))
            }
            1 => {
                let vars = self.unnarrowed(Ty::Lvl);
                let l = (!vars.is_empty()).then(|| self.rng.pick(&vars).name.clone())?;
                // Comparing with a literal narrows `l`, and a narrowed `Lvl` used as `Lvl`
                // fails VIR verification (bug narrowed-literal-widen): compare its text.
                Some(format!("(`${{{l}}}` {op} \"{}\")", self.rng.pick(&LEVELS)))
            }
            _ => {
                let (a, b) = (self.tag_value(Ty::Col), self.tag_value(Ty::Col));
                Some(format!("({a} {op} {b})"))
            }
        }
    }

    /// How a tagged value is printed: the union as JSON, itself or its discriminant.
    pub(super) fn printable_tagged(&mut self, ty: Ty) -> String {
        match ty {
            Ty::Shape => match self.pick_var(ty) {
                Some(v) => match self.rng.below(3) {
                    0 => format!("JSON.stringify({v})"),
                    1 => format!("{v}.kind"),
                    _ => v,
                },
                None => "\"none\"".into(),
            },
            Ty::Col if self.rng.chance(30) => {
                format!("JSON.stringify([{}])", self.tag_value(ty))
            }
            _ => self.tag_value(ty),
        }
    }
}
