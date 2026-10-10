//! Straight-line statements: declarations, assignments, collection and object mutation, prints.
//!
//! Every statement keeps the generator's invariants on its own line (integers reduced `% 10007`,
//! strings cut to 40 characters), so deleting any other line can't produce a false mismatch.

use super::scope::Ty;
use super::Gen;

/// Expression depth for statement operands.
const D: u32 = 3;

impl Gen {
    /// One random simple statement.
    pub(super) fn simple_stmt(&mut self) {
        match self.rng.below(20) {
            0..=4 => self.decl_stmt(),
            5..=8 => self.assign_stmt(),
            9..=11 => self.array_stmt(),
            12 => self.map_stmt(),
            13..=14 => self.object_stmt(),
            15 => self.void_call_stmt(),
            _ => self.print_stmt(),
        }
    }

    /// `const`/`let` of a random type, initialized with a fresh value.
    pub(super) fn decl_stmt(&mut self) {
        let mut tys = vec![
            Ty::Int,
            Ty::Int,
            Ty::Float,
            Ty::Bool,
            Ty::Str,
            Ty::Str,
            Ty::IntArr,
            Ty::StrArr,
        ];
        tys.extend([Ty::FloatArr, Ty::Map, Ty::Union, Ty::IntFn, Ty::ObjArr]);
        tys.extend([Ty::OptInt, Ty::Rec, Ty::Shape, Ty::Lvl, Ty::Col]);
        tys.extend((0..self.classes.len()).map(Ty::Obj));
        let ty = *self.rng.pick(&tys);
        let init = match ty.elem() {
            // The annotation types an empty literal.
            Some(_) if self.rng.chance(15) => "[]".to_string(),
            _ => self.owned(ty, D),
        };
        let rebind = ty != Ty::IntFn && self.rng.chance(60);
        let (kw, prefix) = if rebind { ("let", "v") } else { ("const", "c") };
        let name = self.bind(prefix, ty, rebind);
        // A stored class instance is declared with its class type; `K0` also accepts a `K1`.
        let ann = match ty {
            Ty::Obj(c) if c > 0 && self.rng.chance(30) => {
                self.scope.retype_last(Ty::Obj(0));
                "K0".to_string()
            }
            _ => ty.annotation(),
        };
        if ann.is_empty() {
            self.line(&format!("{kw} {name} = {init};"));
        } else {
            self.line(&format!("{kw} {name}: {ann} = {init};"));
        }
    }

    fn assign_stmt(&mut self) {
        let vars: Vec<_> = [
            Ty::Int,
            Ty::Float,
            Ty::Bool,
            Ty::Str,
            Ty::Union,
            Ty::IntArr,
            Ty::StrArr,
            Ty::OptInt,
            Ty::Rec,
            Ty::Shape,
            Ty::Lvl,
            Ty::Col,
        ]
        .into_iter()
        .flat_map(|t| self.scope.of_type(t))
        .filter(|v| v.rebind)
        .collect();
        if vars.is_empty() {
            return self.decl_stmt();
        }
        let v = self.rng.pick(&vars).clone();
        if v.ty == Ty::Int && self.rng.chance(30) {
            let text = self.compound_int(&v.name);
            return self.line(&text);
        }
        let rhs = match v.ty {
            Ty::Int => {
                let e = self.int(D);
                self.reduce(&e)
            }
            Ty::Str => format!("({}).slice(0, 40)", self.string(D).text),
            other => self.owned(other, D),
        };
        let op = match v.ty {
            Ty::Float if self.rng.chance(40) => *self.rng.pick(&["+=", "-=", "*=", "/="]),
            _ => "=",
        };
        self.line(&format!("{} {op} {rhs};", v.name));
    }

    fn array_stmt(&mut self) {
        let vars: Vec<_> = [Ty::IntArr, Ty::StrArr, Ty::FloatArr, Ty::ObjArr]
            .into_iter()
            .flat_map(|t| self.scope.of_type(t))
            .filter(|v| v.contents)
            .collect();
        if vars.is_empty() {
            return self.decl_stmt();
        }
        let v = self.rng.pick(&vars).clone();
        let elem = v.ty.elem().expect("ICE: array type");
        let mark = self.scope.exclude(&v.name);
        let text = match self.rng.below(6) {
            0..=2 => format!("{}.push({});", v.name, self.owned(elem, D)),
            3 => format!("{}.pop();", v.name),
            4 if v.ty == Ty::ObjArr => {
                let k = self.int(D);
                self.guarded_index(&v.name, &format!("bump({k})"))
            }
            4 => {
                let value = self.owned(elem, D);
                self.guarded_index(&v.name, &format!("= {value}"))
            }
            _ if v.ty == Ty::StrArr && self.rng.chance(50) => format!("{}.sort();", v.name),
            _ => format!("{}.reverse();", v.name),
        };
        self.scope.restore(mark);
        self.line(&text);
    }

    /// `if (xs.length > 0) { xs[i]<suffix>; }` on one line (`suffix` is `= v` or `.method()`).
    fn guarded_index(&mut self, xs: &str, suffix: &str) -> String {
        let i = self.int(D);
        let sep = if suffix.starts_with('=') { " " } else { "." };
        format!(
            "if ({xs}.length > 0) {{ {xs}[(({i} + 20000) as usize) % ({xs}.length as usize)]{sep}{suffix}; }}"
        )
    }

    fn map_stmt(&mut self) {
        let Some(m) = self.pick_var(Ty::Map) else {
            return self.decl_stmt();
        };
        let mark = self.scope.exclude(&m);
        let text = if self.rng.chance(75) {
            let (k, v) = (self.owned_string(D), self.int(D));
            format!("{m}.set({k}, {v});")
        } else {
            format!("{m}.delete({});", self.string(D).text)
        };
        self.scope.restore(mark);
        self.line(&text);
    }

    fn object_stmt(&mut self) {
        if self.rng.chance(25) {
            if let Some(r) = self.scope.of_type(Ty::Rec).into_iter().find(|v| v.contents) {
                let text = if r.rebind && self.rng.chance(30) {
                    // Rebuild through spread: the old value is consumed, then replaced.
                    let e = self.int(D);
                    format!("{0} = {{ ...{0}, x: {1} }};", r.name, self.reduce(&e))
                } else if self.rng.chance(50) {
                    let e = self.int(D);
                    format!("{}.x = {};", r.name, self.reduce(&e))
                } else {
                    format!("{}.s = ({}).slice(0, 40);", r.name, self.string(D).text)
                };
                return self.line(&text);
            }
        }
        let objs: Vec<_> = self
            .scope
            .objects()
            .into_iter()
            .filter(|v| v.contents)
            .collect();
        if objs.is_empty() {
            return self.decl_stmt();
        }
        let o = self.rng.pick(&objs).clone();
        let mark = self.scope.exclude(&o.name);
        let text = match self.rng.below(4) {
            0 => {
                let e = self.int(D);
                format!("{}.a = {};", o.name, self.reduce(&e))
            }
            3 if self.rng.chance(40) => {
                // Getter + setter together (`twice` stays within range: the setter halves).
                match self.rng.chance(50) {
                    true => format!("{}.twice += {};", o.name, self.int(D)),
                    false => format!("{}.twice++;", o.name),
                }
            }
            3 => format!("{}.twice = {};", o.name, self.int(D)),
            1 => format!("{}.s = ({}).slice(0, 40);", o.name, self.string(D).text),
            _ => format!("{}.bump({});", o.name, self.int(D)),
        };
        self.scope.restore(mark);
        self.line(&text);
    }

    /// `console.log(...)` of 1–3 printable values.
    pub(super) fn print_stmt(&mut self) {
        let n = self.rng.range(1, 3);
        let args: Vec<String> = (0..n).map(|_| self.printable(D)).collect();
        self.line(&format!("console.log({});", args.join(", ")));
    }
}
