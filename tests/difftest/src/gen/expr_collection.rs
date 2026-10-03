//! Array and `Map` expressions, including the callbacks of array methods.
//!
//! Indexing is always guarded (`xs.length > 0 ? xs[i] : d`, with `i` made non-negative and reduced
//! by the length): an out-of-bounds read panics in Velt but is `undefined` in JS. The receiver of
//! an array method is hidden inside its callback, so callbacks never alias the array they run on.

use super::expr_text::Expr;
use super::scope::Ty;
use super::Gen;

/// Keeps `usize` indices non-negative: generated ints are > -20000 (see `expr_scalar`).
const INDEX_OFFSET: i64 = 20000;

impl Gen {
    /// An array expression of array type `ty`.
    pub(super) fn array(&mut self, ty: Ty, d: u32) -> Expr {
        let vars = self.scope.of_type(ty);
        if !vars.is_empty() && (d == 0 || self.rng.chance(40)) {
            return Expr::place(self.rng.pick(&vars).name.clone());
        }
        if d == 0 || ty == Ty::ObjArr {
            return Expr::fresh(self.array_literal(ty, d.saturating_sub(1)));
        }
        let d = d - 1;
        match self.rng.below(6) {
            0 => match self.promise_all(ty, d) {
                Some(all) if self.rng.chance(40) => Expr::fresh(all),
                _ => Expr::fresh(self.array_literal(ty, d)),
            },
            1 => self.array_map(ty, d),
            2 => {
                let xs = self.array(ty, d).text;
                let keep = self.callback(&xs, ty, |g, d| g.boolean(d), d);
                Expr::fresh(format!("{xs}.filter({keep})"))
            }
            3 => {
                let (xs, a, b) = (
                    self.array(ty, d).text,
                    self.rng.range(-4, 4),
                    self.rng.range(-4, 6),
                );
                Expr::fresh(format!("{xs}.slice({a}, {b})"))
            }
            4 => {
                let (xs, ys) = (self.array(ty, d).text, self.array(ty, d).text);
                if self.rng.chance(50) {
                    Expr::fresh(format!("{xs}.concat({ys})"))
                } else {
                    let e = self.owned(ty.elem().expect("ICE: array type"), d);
                    Expr::fresh(format!("[...{xs}, {e}]"))
                }
            }
            _ => self.array_derived(ty, d),
        }
    }

    /// A non-empty literal (Velt can't infer the element type of an unannotated `[]`); object
    /// arrays start with a `K0`, since Velt types a literal by its first element.
    pub(super) fn array_literal(&mut self, ty: Ty, d: u32) -> String {
        let elem = ty.elem().expect("ICE: array type");
        let n = self.rng.range(1, 4);
        let items: Vec<String> = (0..n)
            .map(|i| match (ty, i) {
                (Ty::ObjArr, 0) => self.new_object(0, d),
                (Ty::ObjArr, _) => {
                    let c = self.rng.below(self.classes.len());
                    self.new_object(c, d)
                }
                _ => self.owned(elem, d),
            })
            .collect();
        format!("[{}]", items.join(", "))
    }

    /// `xs.map(...)` producing an array of type `ty` from a source array of any element type.
    fn array_map(&mut self, ty: Ty, d: u32) -> Expr {
        let src = *self.rng.pick(&[Ty::IntArr, Ty::StrArr, Ty::FloatArr]);
        let xs = self.array(src, d).text;
        let f = match ty {
            Ty::IntArr => self.callback(&xs, src, |g, d| g.int(d), d),
            Ty::StrArr => self.callback(&xs, src, |g, d| g.owned_string(d), d),
            _ => self.callback(&xs, src, |g, d| g.float(d), d),
        };
        Expr::fresh(format!("{xs}.map({f})"))
    }

    /// Arrays that come from other values: `split`, number-to-string maps.
    fn array_derived(&mut self, ty: Ty, d: u32) -> Expr {
        match ty {
            Ty::StrArr => {
                let s = self.string(d).text;
                let sep = self
                    .rng
                    .pick(&["\",\"", "\" \"", "\"-\"", "\"\"", "\"ab\""]);
                Expr::fresh(format!("{s}.split({sep})"))
            }
            Ty::IntArr => {
                let s = self.string(d).text;
                Expr::fresh(format!("{s}.split(\",\").map((w) => (w.length as i64))"))
            }
            _ => Expr::fresh(self.array_literal(ty, d)),
        }
    }

    /// An arrow function `(p) => body` over the elements of `receiver` (of array type `arr`); the
    /// receiver is hidden inside the body.
    pub(super) fn callback(
        &mut self,
        receiver: &str,
        arr: Ty,
        body: impl FnOnce(&mut Gen, u32) -> String,
        d: u32,
    ) -> String {
        let root = receiver
            .split(['.', '[', '('])
            .next()
            .unwrap_or(receiver)
            .to_string();
        let mark = self.scope.exclude(&root);
        self.scope.push();
        self.scope.closures += 1;
        let p = self.scope.fresh("p");
        self.scope
            .declare(&p, arr.elem().expect("ICE: array type"), false, false);
        let text = body(self, d);
        self.scope.closures -= 1;
        self.scope.pop();
        self.scope.restore(mark);
        format!("({p}) => {text}")
    }

    /// A guarded element read: `(xs.length > 0 ? xs[i]<then> : fallback)`. Non-Copy elements
    /// need a `then` that yields a fresh value (e.g. `.calc(k)`) or a template wrap (`wrap`),
    /// since the ternary would otherwise move the element out of the array.
    pub(super) fn index_read(
        &mut self,
        xs: &str,
        then: &str,
        wrap: bool,
        fallback: String,
        d: u32,
    ) -> String {
        let i = self.int(d);
        let elem = format!("{xs}[(({i} + {INDEX_OFFSET}) as usize) % {xs}.length]{then}");
        let elem = if wrap { format!("`${{{elem}}}`") } else { elem };
        format!("({xs}.length > 0 ? {elem} : {fallback})")
    }

    /// An `i64` computed from a collection, or `None` when no collection is in scope.
    pub(super) fn int_from_collection(&mut self, d: u32) -> Option<String> {
        match self.rng.below(4) {
            0 => {
                let m = self.pick_var(Ty::Map)?;
                Some(if self.rng.chance(50) {
                    format!("({m}.size as i64)")
                } else {
                    format!("({m}.get({}) ?? {})", self.string(d).text, self.int(d))
                })
            }
            1 => self.int_from_object(d),
            2 => self.int_from_union_or_fn(d),
            _ => Some(self.int_from_array(d)),
        }
    }

    fn int_from_array(&mut self, d: u32) -> String {
        let ty = *self.rng.pick(&[Ty::IntArr, Ty::StrArr, Ty::FloatArr]);
        let xs = self.array(ty, d).text;
        match (self.rng.below(5), ty) {
            (0, _) => format!("({xs}.length as i64)"),
            (1, Ty::IntArr) => format!("{xs}.indexOf({})", self.int(d)),
            (1, Ty::StrArr) => format!("{xs}.indexOf({})", self.string(d).text),
            (2, Ty::IntArr) => {
                let fallback = self.int(d);
                self.index_read(&xs, "", false, fallback, d)
            }
            (3, Ty::IntArr) => {
                let root = xs.split(['.', '[']).next().unwrap_or(&xs).to_string();
                let mark = self.scope.exclude(&root);
                let (acc, p) = (self.scope.fresh("acc"), self.scope.fresh("p"));
                self.scope.push();
                self.scope.declare(&acc, Ty::Int, false, false);
                self.scope.declare(&p, Ty::Int, false, false);
                self.scope.closures += 1;
                let body = self.int(d);
                self.scope.closures -= 1;
                self.scope.pop();
                self.scope.restore(mark);
                let step = self.reduce(&format!("{acc} + {body}"));
                format!("{xs}.reduce(({acc}, {p}) => {step}, {})", self.int(d))
            }
            _ => {
                let pred = self.callback(&xs, ty, |g, d| g.boolean(d), d);
                format!("{xs}.findIndex({pred})")
            }
        }
    }

    /// A `bool` computed from a collection.
    pub(super) fn bool_from_collection(&mut self, d: u32) -> Option<String> {
        if self.rng.chance(20) {
            let m = self.pick_var(Ty::Map)?;
            return Some(format!("{m}.has({})", self.string(d).text));
        }
        let ty = *self.rng.pick(&[Ty::IntArr, Ty::StrArr, Ty::FloatArr]);
        let xs = self.array(ty, d).text;
        Some(match (self.rng.below(3), ty) {
            (0, Ty::IntArr) => format!("{xs}.includes({})", self.int(d)),
            (0, Ty::StrArr) => format!("{xs}.includes({})", self.string(d).text),
            (1, _) => {
                let m = *self.rng.pick(&["some", "every"]);
                let pred = self.callback(&xs, ty, |g, d| g.boolean(d), d);
                format!("{xs}.{m}({pred})")
            }
            _ => format!("({xs}.length == 0)"),
        })
    }

    /// A `string` computed from a collection or object.
    pub(super) fn string_from_collection(&mut self, d: u32) -> Option<Expr> {
        match self.rng.below(5) {
            0 => {
                let ty = *self
                    .rng
                    .pick(&[Ty::StrArr, Ty::StrArr, Ty::IntArr, Ty::FloatArr]);
                let xs = self.array(ty, d).text;
                let sep = self.rng.pick(&["\",\"", "\"\"", "\" | \""]);
                Some(Expr::fresh(match self.rng.chance(20) {
                    true => format!("{xs}.join()"),
                    false => format!("{xs}.join({sep})"),
                }))
            }
            1 => {
                let xs = self.array(Ty::StrArr, d).text;
                let fallback = self.string_lit();
                Some(Expr::fresh(self.index_read(&xs, "", true, fallback, d)))
            }
            2 => {
                let ty =
                    *self
                        .rng
                        .pick(&[Ty::IntArr, Ty::StrArr, Ty::FloatArr, Ty::Map, Ty::ObjArr]);
                {
                    // JSON escapes (`\"`) would reach console.log of containers: bug
                    // console-log-string-escapes.
                    let json = self.json_of(ty, d);
                    Some(Expr::fresh(format!("{json}.replaceAll(\"\\\\\", \"/\")")))
                }
            }
            3 => self.string_from_object(d),
            _ => {
                let u = self
                    .pick_var(Ty::Union)
                    .filter(|_| self.scope.closures == 0)?;
                let lit = self.string_lit();
                Some(Expr::fresh(format!(
                    "(typeof {u} === \"string\" ? {u}.toUpperCase() : {lit})"
                )))
            }
        }
    }

    /// `JSON.stringify` of a value of type `ty` (maps: their key list).
    pub(super) fn json_of(&mut self, ty: Ty, d: u32) -> String {
        match ty {
            Ty::Map => match self.pick_var(Ty::Map) {
                Some(m) => {
                    let part = self.rng.pick(&["keys", "values"]);
                    format!("JSON.stringify([...{m}.{part}()])")
                }
                None => "\"[]\"".into(),
            },
            // `K0` has a private field, so it has no JSON form in Velt (it is not part of the
            // TS-compatible subset): stringify one of its public fields instead.
            Ty::ObjArr => {
                let field = *self.rng.pick(&["a", "s"]);
                let xs = self.array(ty, d).text;
                let o = self.scope.fresh("p");
                format!("JSON.stringify({xs}.map(({o}) => {o}.{field}))")
            }
            _ => format!("JSON.stringify({})", self.array(ty, d).text),
        }
    }

    /// The name of a random visible variable of type `ty`.
    pub(super) fn pick_var(&mut self, ty: Ty) -> Option<String> {
        let vars = self.scope.of_type(ty);
        (!vars.is_empty()).then(|| self.rng.pick(&vars).name.clone())
    }
}
