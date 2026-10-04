//! Types of the generated subset and the lexical scope the generator draws variables from.
//!
//! Besides visibility, the scope tracks what the Velt ownership rules allow: which bindings may be
//! reassigned, whose contents may be modified (a `for...of` element's fields may not), and which
//! places are *excluded* while an argument list is generated (exclusive access: `xs.push(f(xs))`).

/// A type of the shared subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    /// `i64`, kept within ±10007 so arithmetic never overflows and JS's 32-bit bitwise ops agree.
    Int,
    /// `f64` (any value, including NaN and ±Infinity).
    Float,
    /// `bool`.
    Bool,
    /// `string`, ASCII or not (lengths and positions are UTF-16 code units on both sides).
    Str,
    /// `i64[]`.
    IntArr,
    /// `string[]`.
    StrArr,
    /// `f64[]`.
    FloatArr,
    /// `Map<string, i64>`.
    Map,
    /// An instance of generated class `K<n>`.
    Obj(usize),
    /// `K0[]` (instances of any class of the hierarchy).
    ObjArr,
    /// `i64 | string`.
    Union,
    /// `(x: i64) => i64`, a stored closure.
    IntFn,
    /// `i64 | null`.
    OptInt,
    /// `{ x: i64, s: string }`, an object literal (declared without annotation: Velt has no
    /// object type literals).
    Rec,
    /// `Sh`, the discriminated union `{ kind: "pt"; x } | { kind: "box"; x; s } | { kind: "nil" }`.
    Shape,
    /// `Lvl`, the literal union `"lo" | "mid" | "hi"`.
    Lvl,
    /// `Col`, a string enum.
    Col,
}

impl Ty {
    /// The Velt/TS annotation.
    pub fn annotation(self) -> String {
        match self {
            Ty::Int => "i64".into(),
            Ty::Float => "f64".into(),
            Ty::Bool => "boolean".into(),
            Ty::Str => "string".into(),
            Ty::IntArr => "i64[]".into(),
            Ty::StrArr => "string[]".into(),
            Ty::FloatArr => "f64[]".into(),
            Ty::Map => "Map<string, i64>".into(),
            Ty::Obj(c) => format!("K{c}"),
            Ty::ObjArr => "K0[]".into(),
            Ty::Union => "i64 | string".into(),
            Ty::IntFn => "(x: i64) => i64".into(),
            Ty::OptInt => "i64 | null".into(),
            Ty::Rec => String::new(),
            Ty::Shape => "Sh".into(),
            Ty::Lvl => "Lvl".into(),
            Ty::Col => "Col".into(),
        }
    }

    /// Numbers and bools: copied, never moved.
    pub fn is_copy(self) -> bool {
        matches!(
            self,
            Ty::Int | Ty::Float | Ty::Bool | Ty::OptInt | Ty::Lvl | Ty::Col
        )
    }

    /// Element type of an array type.
    pub fn elem(self) -> Option<Ty> {
        match self {
            Ty::IntArr => Some(Ty::Int),
            Ty::StrArr => Some(Ty::Str),
            Ty::FloatArr => Some(Ty::Float),
            Ty::ObjArr => Some(Ty::Obj(0)),
            _ => None,
        }
    }
}

/// A binding (or a `this.field` pseudo-binding inside methods).
#[derive(Clone, Debug)]
pub struct Var {
    /// Source text that names it (`v3`, `this.a`).
    pub name: String,
    /// Its type.
    pub ty: Ty,
    /// May be reassigned (`let`, not a param or loop element).
    pub rebind: bool,
    /// Its contents may be modified (push, field writes). False for `for...of` elements.
    pub contents: bool,
    /// Only valid through flow narrowing (`v.s` in `case "box":`, a `typeof`-narrowed union):
    /// hidden in closures, which Velt doesn't narrow captured locals for (gap
    /// `narrow-captured-union`).
    pub narrowed: bool,
}

/// Nested frames of bindings plus the exclusion list.
pub struct Scope {
    frames: Vec<Vec<Var>>,
    excluded: Vec<String>,
    /// Only `const` Copy bindings are visible (bodies of stored closures, which capture by copy).
    pub copy_only: bool,
    /// Nesting depth of arrow functions: Velt doesn't narrow captured unions (gap
    /// `narrow-captured-union`), so narrowing is only generated outside them.
    pub closures: usize,
    next_id: usize,
}

impl Scope {
    /// An empty scope (one frame).
    pub fn new() -> Self {
        Scope {
            frames: vec![Vec::new()],
            excluded: Vec::new(),
            copy_only: false,
            closures: 0,
            next_id: 0,
        }
    }

    /// Starts a function body: no outer locals are visible (fresh names keep counting).
    pub fn enter_function(&mut self) -> Vec<Vec<Var>> {
        std::mem::replace(&mut self.frames, vec![Vec::new()])
    }

    /// Ends a function body, restoring the frames returned by [`Scope::enter_function`].
    pub fn leave_function(&mut self, saved: Vec<Vec<Var>>) {
        self.frames = saved;
    }

    /// Opens a block.
    pub fn push(&mut self) {
        self.frames.push(Vec::new());
    }

    /// Closes a block.
    pub fn pop(&mut self) {
        self.frames.pop();
    }

    /// A fresh identifier with the given prefix.
    pub fn fresh(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}{}", self.next_id)
    }

    /// Adds a binding to the innermost frame.
    pub fn declare(&mut self, name: &str, ty: Ty, rebind: bool, contents: bool) {
        self.declare_var(Var {
            name: name.to_string(),
            ty,
            rebind,
            contents,
            narrowed: false,
        });
    }

    /// Adds a read-only binding that exists through narrowing only (see [`Var::narrowed`]).
    pub fn declare_narrowed(&mut self, name: &str, ty: Ty) {
        self.declare_var(Var {
            name: name.to_string(),
            ty,
            rebind: false,
            contents: false,
            narrowed: true,
        });
    }

    fn declare_var(&mut self, var: Var) {
        self.frames
            .last_mut()
            .expect("ICE: scope has a frame")
            .push(var);
    }

    /// Changes the type of the most recent binding (a subclass instance declared as its base).
    pub fn retype_last(&mut self, ty: Ty) {
        if let Some(v) = self.frames.last_mut().and_then(|f| f.last_mut()) {
            v.ty = ty;
        }
    }

    /// Visible, non-excluded bindings of type `ty`.
    pub fn of_type(&self, ty: Ty) -> Vec<Var> {
        self.visible().filter(|v| v.ty == ty).cloned().collect()
    }

    /// Visible, non-excluded bindings of any class type.
    pub fn objects(&self) -> Vec<Var> {
        self.visible()
            .filter(|v| matches!(v.ty, Ty::Obj(_)))
            .cloned()
            .collect()
    }

    /// Bindings of the innermost block (printed at the end of `main`).
    pub fn innermost(&self) -> Vec<Var> {
        self.frames.last().cloned().unwrap_or_default()
    }

    /// Hides `name` (and everything reached through it) until [`Scope::restore`].
    pub fn exclude(&mut self, name: &str) -> usize {
        let mark = self.excluded.len();
        self.excluded.push(name.to_string());
        mark
    }

    /// Undoes exclusions made since `mark`.
    pub fn restore(&mut self, mark: usize) {
        self.excluded.truncate(mark);
    }

    /// Bindings in scope: not excluded, not shadowed by a later binding of the same name (a
    /// narrowed local shadows the union it narrows), and allowed in the current closure.
    fn visible(&self) -> impl Iterator<Item = &Var> {
        let all: Vec<&Var> = self.frames.iter().flatten().collect();
        let live: Vec<&Var> = all
            .iter()
            .enumerate()
            .filter(|(i, v)| !all[i + 1..].iter().any(|w| w.name == v.name))
            .map(|(_, v)| *v)
            .collect();
        live.into_iter().filter(move |v| {
            let root = v.name.split(['.', '[']).next().unwrap_or("");
            let hidden = self.excluded.iter().any(|x| x == &v.name || x == root);
            let captured_ok = !self.copy_only || (!v.rebind && v.ty.is_copy());
            let narrowing_ok = self.closures == 0 || !v.narrowed;
            !hidden && captured_ok && narrowing_ok
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusion_hides_paths_through_a_root() {
        let mut s = Scope::new();
        s.declare("o", Ty::Obj(0), false, true);
        s.declare("o.a", Ty::Int, true, true);
        s.declare("n", Ty::Int, true, true);
        let mark = s.exclude("o");
        assert_eq!(s.of_type(Ty::Int).len(), 1);
        s.restore(mark);
        assert_eq!(s.of_type(Ty::Int).len(), 2);
    }

    #[test]
    fn narrowed_bindings_are_hidden_in_closures() {
        let mut s = Scope::new();
        s.declare_narrowed("v.x", Ty::Int);
        assert_eq!(s.of_type(Ty::Int).len(), 1);
        s.closures += 1;
        assert!(s.of_type(Ty::Int).is_empty());
    }

    #[test]
    fn later_bindings_shadow_earlier_ones() {
        let mut s = Scope::new();
        s.declare("u", Ty::Union, false, true);
        s.push();
        s.declare_narrowed("u", Ty::Int);
        assert!(s.of_type(Ty::Union).is_empty());
        assert_eq!(s.of_type(Ty::Int).len(), 1);
        s.pop();
        assert_eq!(s.of_type(Ty::Union).len(), 1);
    }

    #[test]
    fn copy_only_sees_const_numbers() {
        let mut s = Scope::new();
        s.declare("c", Ty::Int, false, true);
        s.declare("l", Ty::Int, true, true);
        s.declare("t", Ty::Str, false, true);
        s.copy_only = true;
        assert_eq!(s.of_type(Ty::Int).len(), 1);
        assert!(s.of_type(Ty::Str).is_empty());
    }
}
