//! Class instances, unions and stored closures; plus the two context-driven entry points,
//! [`Gen::owned`] (a movable value) and [`Gen::printable`] (a `console.log` argument).

use super::expr_text::Expr;
use super::scope::Ty;
use super::Gen;

impl Gen {
    /// A value of type `ty` that may be moved into its destination: never a place, so no program
    /// depends on Velt move semantics (JS would alias, Velt would move).
    pub(super) fn owned(&mut self, ty: Ty, d: u32) -> String {
        match ty {
            Ty::Int => self.int(d),
            Ty::Float => self.float(d),
            Ty::Bool => self.boolean(d),
            Ty::Str => self.owned_string(d),
            // `slice` clones elements in Velt but shares them in JS: object arrays are always
            // built fresh so no instance is reachable from two arrays.
            Ty::ObjArr => self.array_literal(ty, d),
            Ty::IntArr | Ty::StrArr | Ty::FloatArr => {
                let e = self.array(ty, d);
                if e.place {
                    format!("{}.slice(0)", e.text)
                } else {
                    e.text
                }
            }
            Ty::Map => "new Map<string, i64>()".into(),
            Ty::Obj(c) => self.new_object(c, d),
            Ty::Union => {
                if self.rng.chance(50) {
                    self.int(d)
                } else {
                    self.owned_string(d)
                }
            }
            Ty::IntFn => self.stored_closure(d),
            Ty::OptInt => self.opt_int(d),
            Ty::Rec => self.record_literal(d),
            Ty::Shape => self.shape_literal(d),
            Ty::Lvl | Ty::Col => self.tag_value(ty),
        }
    }

    /// `new K<c>(...)` with every field initialized.
    pub(super) fn new_object(&mut self, c: usize, d: u32) -> String {
        let fields = self.classes[c].fields.clone();
        let args: Vec<String> = fields.iter().map(|(_, ty)| self.owned(*ty, d)).collect();
        format!("new K{c}({})", args.join(", "))
    }

    /// `(x: i64): i64 => ...` that only reads `const` numbers: a stored closure captures by copy in
    /// Velt but by reference in JS, which agree only for bindings that never change.
    fn stored_closure(&mut self, d: u32) -> String {
        let saved = std::mem::replace(&mut self.scope.copy_only, true);
        self.scope.push();
        let x = self.scope.fresh("x");
        self.scope.declare(&x, Ty::Int, false, false);
        self.scope.closures += 1;
        let body = self.int(d);
        self.scope.closures -= 1;
        self.scope.pop();
        self.scope.copy_only = saved;
        format!("({x}: i64): i64 => {}", self.reduce(&body))
    }

    /// An `i64` read from an object or object array.
    pub(super) fn int_from_object(&mut self, d: u32) -> Option<String> {
        if self.rng.chance(30) {
            let os = self.pick_var(Ty::ObjArr)?;
            return Some(match self.rng.below(3) {
                0 => format!("({os}.length as i64)"),
                1 => {
                    let (k, fallback) = (self.int(d), self.int(d));
                    self.index_read(&os, &format!(".calc({k})"), false, fallback, d)
                }
                _ => format!("{os}.reduce((acc, o) => {}, 0)", self.reduce("acc + o.a")),
            });
        }
        let objs = self.scope.objects();
        if objs.is_empty() {
            return None;
        }
        let o = self.rng.pick(&objs).clone();
        Some(match (self.rng.below(3), o.ty) {
            (0, _) => format!("{}.a", o.name),
            (1, Ty::Obj(1)) => format!("{}.b", o.name),
            _ => format!("{}.calc({})", o.name, self.int(d)),
        })
    }

    /// A string read from an object (`o.s` is a place, the getter `o.tag` a fresh value).
    pub(super) fn string_from_object(&mut self, _d: u32) -> Option<Expr> {
        let objs = self.scope.objects();
        if objs.is_empty() {
            return None;
        }
        let o = self.rng.pick(&objs).name.clone();
        Some(if self.rng.chance(50) {
            Expr::place(format!("{o}.s"))
        } else {
            Expr::fresh(format!("{o}.tag"))
        })
    }

    /// An `i64` from a narrowed union or a stored closure call.
    pub(super) fn int_from_union_or_fn(&mut self, d: u32) -> Option<String> {
        if self.rng.chance(50) {
            let u = self
                .pick_var(Ty::Union)
                .filter(|_| self.scope.closures == 0)?;
            return Some(format!(
                "(typeof {u} === \"number\" ? {u} : {})",
                self.int(d)
            ));
        }
        let f = self.pick_var(Ty::IntFn)?;
        Some(format!("{f}({})", self.int(d)))
    }

    /// A `console.log` argument of a random type, printed in a form both languages agree on.
    pub(super) fn printable(&mut self, d: u32) -> String {
        let ty = self.printable_type();
        match ty {
            Ty::Int => self.int(d),
            Ty::Bool => self.boolean(d),
            Ty::Str => self.string(d).text,
            // `console.log(-0)` prints `-0` in Node but `0` in Velt; templates agree.
            Ty::Float => format!("`${{{}}}`", self.float(d)),
            Ty::IntArr | Ty::StrArr | Ty::ObjArr if self.rng.chance(60) => {
                format!("{}.slice(0, 6)", self.array(ty, d).text)
            }
            Ty::Obj(_) | Ty::Union | Ty::OptInt => {
                self.pick_var(ty).unwrap_or_else(|| "\"none\"".into())
            }
            Ty::Shape | Ty::Lvl | Ty::Col => self.printable_tagged(ty),
            Ty::Rec => match self.pick_var(ty) {
                Some(r) if self.rng.chance(50) => format!("JSON.stringify({r})"),
                Some(r) => r,
                None => "\"none\"".into(),
            },
            _ => self.json_of(ty, d),
        }
    }

    fn printable_type(&mut self) -> Ty {
        let mut tys = vec![
            Ty::Int,
            Ty::Int,
            Ty::Str,
            Ty::Str,
            Ty::Float,
            Ty::Bool,
            Ty::IntArr,
            Ty::StrArr,
        ];
        tys.extend([
            Ty::FloatArr,
            Ty::Map,
            Ty::Union,
            Ty::ObjArr,
            Ty::OptInt,
            Ty::Rec,
            Ty::Shape,
            Ty::Lvl,
            Ty::Col,
        ]);
        tys.extend(self.scope.objects().iter().map(|v| v.ty));
        *self.rng.pick(&tys)
    }
}
