//! Top-level declarations: the class hierarchy, helper functions (and calls to them) and `main`.

use super::scope::Ty;
use super::{ClassInfo, FnSig, Gen};

/// Helper parameter types (objects are exercised through locals instead).
const PARAM_TYPES: [Ty; 10] = [
    Ty::Int,
    Ty::Int,
    Ty::Float,
    Ty::Str,
    Ty::Bool,
    Ty::IntArr,
    Ty::StrArr,
    Ty::Shape,
    Ty::Lvl,
    Ty::Col,
];

impl Gen {
    /// `interface Calc`, `class Bad extends Error`, `K0` and (usually) `K1 extends K0`.
    pub(super) fn classes_decl(&mut self) {
        self.out
            .push_str("interface Calc {\n  calc(k: i64): i64;\n}\n\n");
        self.out.push_str(
            "class Bad extends Error {\n  constructor(m: string) {\n    super(m);\n  }\n}\n\n",
        );
        self.classes.push(ClassInfo {
            fields: vec![("a".into(), Ty::Int), ("s".into(), Ty::Str)],
        });
        self.open("class K0 implements Calc {");
        let (limit, p) = (self.rng.range(-9, 99), self.rng.range(0, 9));
        self.line(&format!("static readonly LIMIT: i64 = {limit};"));
        self.line(&format!("private p: i64 = {p};"));
        self.line("a: i64;");
        self.line("s: string;");
        self.open("constructor(a: i64, s: string) {");
        self.line("this.a = a;");
        self.line("this.s = s;");
        self.close("}");
        self.method_bodies(false);
        self.close("}");
        if self.rng.chance(70) {
            let mut fields = self.classes[0].fields.clone();
            fields.push(("b".into(), Ty::Int));
            self.classes.push(ClassInfo { fields });
            self.out.push('\n');
            self.open("class K1 extends K0 {");
            self.line("b: i64;");
            self.open("constructor(a: i64, s: string, b: i64) {");
            self.line("super(a, s);");
            self.line("this.b = b;");
            self.close("}");
            self.method_bodies(true);
            self.close("}");
        }
    }

    /// `calc`, `bump` and `tag` for `K0`; an overriding `calc` for `K1`.
    fn method_bodies(&mut self, sub: bool) {
        let saved = self.scope.enter_function();
        self.scope.declare("this.a", Ty::Int, false, false);
        self.scope.declare("this.s", Ty::Str, false, false);
        if sub {
            self.scope.declare("this.b", Ty::Int, false, false);
        } else {
            // Private: visible in `K0`'s own methods only, not in the subclass.
            self.scope.declare("this.p", Ty::Int, false, false);
        }
        self.scope.declare("K0.LIMIT", Ty::Int, false, false);
        // The getter has no `k`: its template is generated before `k` is in scope.
        let tag = self.template(1);
        self.scope.declare("k", Ty::Int, false, false);
        let prefix = if sub { "override " } else { "" };
        let mut body = self.int(2);
        if sub && self.rng.chance(50) {
            body = format!("super.calc(k) + {body}");
        }
        self.open(&format!("{prefix}calc(k: i64): i64 {{"));
        self.line(&format!("return {};", self.reduce(&body)));
        self.close("}");
        if !sub {
            let m = self.rng.range(1, 9);
            self.open("bump(k: i64) {");
            self.line(&format!(
                "this.a = {};",
                self.reduce(&format!("this.a + k * {m}"))
            ));
            self.close("}");
            self.open("get tag(): string {");
            self.line(&format!("return {tag};"));
            self.close("}");
            self.open("get twice(): i64 {");
            self.line(&format!("return {};", self.reduce("this.a * 2")));
            self.close("}");
            self.open("set twice(v: i64) {");
            self.line("this.a = ((v - (v % 2)) / 2) | 0;");
            self.close("}");
        }
        self.scope.leave_function(saved);
    }

    /// Generic and recursive functions every program may call: `maxOf` (a `Comparable` bound;
    /// `Comparable` is only a type to Node), `countOf` and the bounded recursion `rec`.
    pub(super) fn library_decl(&mut self) {
        let k = self.rng.range(2, 9);
        self.out.push_str(concat!(
            "\nfunction maxOf<T extends Comparable<T>>(a: T, b: T): T {\n",
            "  return a > b ? a : b;\n}\n\n",
            "function countOf<T>(xs: T[], v: T): i64 {\n",
            "  return xs.filter((x) => x == v).length as i64;\n}\n\n",
            "function rec(n: i64, acc: i64): i64 {\n",
            "  if (n <= 0) {\n    return acc;\n  }\n",
        ));
        self.out.push_str(&format!(
            "  return rec(n - 1, {});\n}}\n",
            self.reduce(&format!("acc * {k} + n"))
        ));
    }

    /// A helper `h<n>(...)`: value-returning helpers only read their params; void helpers modify
    /// their first (array) param, which the caller observes (shared object semantics).
    pub(super) fn helper_decl(&mut self) {
        let void = self.rng.chance(25);
        // Async helpers only read their params: async params are moved into the future, so
        // a caller wouldn't see modifications (JS would).
        let is_async = !void && self.rng.chance(30);
        let throws = !void && self.rng.chance(35);
        let ret = if void {
            None
        } else if throws {
            Some(Ty::Int)
        } else {
            Some(
                *self
                    .rng
                    .pick(&[Ty::Int, Ty::Int, Ty::Float, Ty::Str, Ty::Bool]),
            )
        };
        let saved = self.scope.enter_function();
        let mut params = Vec::new();
        if void {
            let ty = *self.rng.pick(&[Ty::IntArr, Ty::StrArr]);
            let name = self.scope.fresh("a");
            self.scope.declare(&name, ty, false, true);
            params.push((ty, name));
        }
        for _ in 0..self.rng.range(if void { 0 } else { 1 }, 3) {
            let ty = *self.rng.pick(&PARAM_TYPES);
            let name = self.scope.fresh("a");
            self.scope.declare(&name, ty, false, false);
            params.push((ty, name));
        }
        let name = format!("h{}", self.funcs.len());
        let default_last = !void
            && matches!(params.last(), Some((Ty::Int | Ty::Str | Ty::Bool, _)))
            && self.rng.chance(30);
        let mut list: Vec<String> = params
            .iter()
            .map(|(t, n)| format!("{n}: {}", t.annotation()))
            .collect();
        if default_last {
            let ty = params.last().expect("ICE: checked above").0;
            let value = match ty {
                Ty::Int => self.rng.range(-5, 50).to_string(),
                Ty::Str => self.string_lit(),
                _ => self.rng.pick(&["true", "false"]).to_string(),
            };
            let last = list.last_mut().expect("ICE: checked above");
            last.push_str(&format!(" = {value}"));
        }
        let ret_ann = match (ret, is_async) {
            (Some(t), true) => format!(": Promise<{}>", t.annotation()),
            (Some(t), false) => format!(": {}", t.annotation()),
            (None, _) => String::new(),
        };
        let kw = if is_async {
            "async function"
        } else {
            "function"
        };
        self.out.push('\n');
        let start = self.out.len();
        self.open(&format!("{kw} {name}({}){ret_ann} {{", list.join(", ")));
        let saved_async = std::mem::replace(&mut self.in_async, is_async);
        if throws {
            let (c, n) = (self.boolean(2), self.int(1));
            let class = *self.rng.pick(&["Error", "Bad"]);
            self.line(&format!(
                "if ({c}) {{ throw new {class}(`bad ${{{n}}}`); }}"
            ));
        }
        self.budget = 4;
        self.ret = Some(ret);
        self.block(4);
        self.ret = None;
        if let Some(ty) = ret {
            let value = self.owned(ty, 3);
            self.line(&format!("return {value};"));
        }
        self.close("}");
        self.in_async = saved_async;
        let body = &self.out[start..];
        let quiet = !body.contains("console.log") && !body.contains("await");
        self.scope.leave_function(saved);
        self.funcs.push(FnSig {
            name,
            params,
            ret,
            throws,
            default_last,
            is_async,
            quiet,
        });
    }

    /// A call of a non-throwing helper returning `ret`, if one exists.
    pub(super) fn call_expr(&mut self, ret: Ty, d: u32) -> Option<String> {
        let sigs: Vec<FnSig> = self
            .funcs
            .iter()
            .filter(|f| f.ret == Some(ret) && !f.throws && self.can_call(f))
            .cloned()
            .collect();
        if sigs.is_empty() {
            return None;
        }
        let sig = self.rng.pick(&sigs).clone();
        Some(self.call_with(&sig, d))
    }

    /// `name(args)`, awaited when `sig` is async.
    pub(super) fn call_with(&mut self, sig: &FnSig, d: u32) -> String {
        let call = self.call_text(sig, d);
        if sig.is_async {
            format!("(await {call})")
        } else {
            call
        }
    }

    /// `name(args)` with arguments of the parameter types (borrowed: places are fine).
    fn call_text(&mut self, sig: &FnSig, d: u32) -> String {
        let omit = usize::from(sig.default_last && self.rng.chance(50));
        let args: Vec<String> = sig.params[..sig.params.len() - omit]
            .iter()
            .map(|(ty, _)| self.argument(*ty, d))
            .collect();
        format!("{}({})", sig.name, args.join(", "))
    }

    /// Async helpers can only be awaited directly in an async body (not inside a closure).
    pub(super) fn can_call(&self, f: &FnSig) -> bool {
        !f.is_async || (self.in_async && self.scope.closures == 0)
    }

    /// `(await Promise.all([h(...), ...]))`: quiet async helpers returning the element type of
    /// `ty`, run concurrently.
    pub(super) fn promise_all(&mut self, ty: Ty, d: u32) -> Option<String> {
        let elem = ty.elem()?;
        let sigs: Vec<FnSig> = self
            .funcs
            .iter()
            .filter(|f| f.is_async && f.quiet && !f.throws && f.ret == Some(elem))
            .cloned()
            .collect();
        if sigs.is_empty() || !self.in_async || self.scope.closures > 0 {
            return None;
        }
        let calls: Vec<String> = (0..self.rng.range(1, 3))
            .map(|_| {
                let sig = self.rng.pick(&sigs).clone();
                self.call_text(&sig, d)
            })
            .collect();
        Some(format!("(await Promise.all([{}]))", calls.join(", ")))
    }

    fn argument(&mut self, ty: Ty, d: u32) -> String {
        match ty {
            Ty::Str => self.string(d).text,
            Ty::IntArr | Ty::StrArr => self.array(ty, d).text,
            // A borrowed union local or a literal of the member.
            Ty::Shape => match self.pick_var(ty) {
                Some(v) if self.rng.chance(60) => v,
                _ => self.shape_literal(d),
            },
            other => self.owned(other, d),
        }
    }

    /// A call of a void helper; its first argument is an array it modifies, excluded from the
    /// other arguments (exclusive access).
    pub(super) fn void_call_stmt(&mut self) {
        let sigs: Vec<FnSig> = self
            .funcs
            .iter()
            .filter(|f| f.ret.is_none())
            .cloned()
            .collect();
        if sigs.is_empty() {
            return self.print_stmt();
        }
        let sig = self.rng.pick(&sigs).clone();
        let target_ty = sig.params[0].0;
        let target = self
            .scope
            .of_type(target_ty)
            .into_iter()
            .filter(|v| v.contents)
            .map(|v| v.name)
            .next()
            .unwrap_or_else(|| self.owned(target_ty, 1));
        let mark = self.scope.exclude(&target);
        let mut args = vec![target];
        args.extend(sig.params[1..].iter().map(|(ty, _)| self.argument(*ty, 2)));
        self.scope.restore(mark);
        self.line(&format!("{}({});", sig.name, args.join(", ")));
    }

    /// `function main()`: a block of statements, then every top-level local is printed.
    pub(super) fn main_decl(&mut self) {
        let saved = self.scope.enter_function();
        self.out.push('\n');
        self.in_async = self.funcs.iter().any(|f| f.is_async);
        let kw = if self.in_async {
            "async function"
        } else {
            "function"
        };
        self.open(&format!("{kw} main() {{"));
        self.budget = self.rng.range(12, 40) as usize;
        self.block(40);
        for v in self.scope.innermost() {
            let shown = match v.ty {
                Ty::Float => format!("`${{{}}}`", v.name),
                Ty::FloatArr => format!("JSON.stringify({})", v.name),
                Ty::Map => format!("JSON.stringify([...{}.keys()])", v.name),
                Ty::IntArr | Ty::StrArr | Ty::ObjArr => format!("{}.slice(0, 6)", v.name),
                Ty::IntFn => format!("{}(3)", v.name),
                _ => v.name.clone(),
            };
            self.line(&format!("console.log(\"{}\", {shown});", v.name));
        }
        if self.rng.chance(10) {
            self.throw_stmt();
        }
        self.close("}");
        self.scope.leave_function(saved);
    }
}
