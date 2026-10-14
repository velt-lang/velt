//! Free value references: the identifiers of a module that name a value nothing in their scope
//! declares, as JavaScript scopes them. Bindings are imports (not `import type`), top-level and
//! block-level declarations (functions, classes, structs, enums, `const`/`let`, the whole block
//! being their scope), parameters, the name of a function expression, `for` and `for … of`
//! variables and `catch` parameters. Types are not values: `typeof x` in a type, interfaces and
//! type aliases neither bind nor refer.
//!
//! The build-time constants of `define` replace exactly these references ([`crate::parse_with`]),
//! as esbuild's `define` does, and the loader reports an ambient constant without a value only
//! when a module refers to it so.

use crate::ast::*;

/// The identifier expressions (and shorthand properties, `{ x }`) of `module` that refer to a
/// name for which `wanted` holds and that nothing in scope declares, in source order.
pub fn free_values<'a>(
    module: &'a Module,
    wanted: &(dyn Fn(&str) -> bool + Sync),
) -> Vec<&'a Ident> {
    crate::on_parser_thread(|| {
        let mut w = Walker {
            wanted,
            scopes: vec![],
            out: vec![],
        };
        let mut top = vec![];
        for item in &module.items {
            w.item_names(item, &mut top);
        }
        w.scopes.push(top);
        for item in &module.items {
            w.item(item);
        }
        let mut out = w.out;
        out.sort_by_key(|id| id.span.lo);
        out
    })
}

struct Walker<'a, 'w> {
    wanted: &'w (dyn Fn(&str) -> bool + Sync),
    /// The wanted names each enclosing scope declares, innermost last.
    scopes: Vec<Vec<&'a str>>,
    out: Vec<&'a Ident>,
}

impl<'a> Walker<'a, '_> {
    fn bind(&self, id: &'a Ident, names: &mut Vec<&'a str>) {
        if (self.wanted)(&id.name) {
            names.push(&id.name);
        }
    }

    fn is_bound(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(&name))
    }

    fn reference(&mut self, id: &'a Ident) {
        if (self.wanted)(&id.name) && !self.is_bound(&id.name) {
            self.out.push(id);
        }
    }

    /// The values `item` declares in its scope.
    fn item_names(&self, item: &'a Item, names: &mut Vec<&'a str>) {
        match &item.kind {
            ItemKind::Import(imp) => {
                if item.exported {
                    return; // A re-export or `export { a }` binds nothing.
                }
                for n in imp.names.iter().filter(|n| !n.type_only) {
                    self.bind(n.alias.as_ref().unwrap_or(&n.name), names);
                }
                if let Some(ns) = &imp.namespace {
                    self.bind(ns, names);
                }
            }
            ItemKind::Function(f) => self.bind(&f.sig.name, names),
            ItemKind::ExternFn(sig) => self.bind(&sig.name, names),
            ItemKind::Struct(t) | ItemKind::Class(t) => self.bind(&t.name, names),
            ItemKind::Enum(e) => self.bind(&e.name, names),
            ItemKind::Var(v) => self.pattern_names(&v.pattern, names),
            ItemKind::Interface(_) | ItemKind::TypeAlias(_) | ItemKind::Extend(_) => {}
        }
    }

    fn pattern_names(&self, p: &'a Pattern, names: &mut Vec<&'a str>) {
        match &p.kind {
            PatternKind::Ident(id) => self.bind(id, names),
            PatternKind::Wildcard => {}
            PatternKind::Object { fields, rest } => {
                fields
                    .iter()
                    .for_each(|(_, p)| self.pattern_names(p, names));
                rest.iter().for_each(|r| self.bind(r, names));
            }
            PatternKind::Array { elems, rest } => {
                elems.iter().for_each(|p| self.pattern_names(p, names));
                rest.iter().for_each(|r| self.bind(r, names));
            }
            PatternKind::Default { pattern, .. } => self.pattern_names(pattern, names),
        }
    }

    /// The default values inside pattern `p`.
    fn pattern_defaults(&mut self, p: &'a Pattern) {
        match &p.kind {
            PatternKind::Ident(_) | PatternKind::Wildcard => {}
            PatternKind::Object { fields, .. } => {
                fields.iter().for_each(|(_, p)| self.pattern_defaults(p));
            }
            PatternKind::Array { elems, .. } => elems.iter().for_each(|p| self.pattern_defaults(p)),
            PatternKind::Default { pattern, value } => {
                self.pattern_defaults(pattern);
                self.expr(value);
            }
        }
    }

    fn item(&mut self, item: &'a Item) {
        match &item.kind {
            ItemKind::Function(f) => self.function(f, None),
            ItemKind::Struct(t) | ItemKind::Class(t) => {
                for field in &t.fields {
                    if let Some(e) = &field.default {
                        self.expr(e);
                    }
                }
                if let Some(ctor) = &t.constructor {
                    self.function(ctor, None);
                }
                t.methods.iter().for_each(|m| self.function(&m.decl, None));
            }
            ItemKind::Interface(i) => {
                for m in &i.methods {
                    if let Some(body) = &m.body {
                        self.body(&m.sig, body, None);
                    }
                }
            }
            ItemKind::Extend(ext) => ext
                .methods
                .iter()
                .for_each(|m| self.function(&m.decl, None)),
            ItemKind::Enum(e) => {
                for v in &e.variants {
                    if let Some(d) = &v.discriminant {
                        self.expr(d);
                    }
                }
            }
            ItemKind::Var(v) => self.var(v),
            ItemKind::TypeAlias(_) | ItemKind::ExternFn(_) | ItemKind::Import(_) => {}
        }
    }

    /// A function, method or constructor; `own` is a function expression's name, which its
    /// body sees.
    fn function(&mut self, f: &'a FnDecl, own: Option<&'a Ident>) {
        self.body(&f.sig, &f.body, own);
    }

    fn body(&mut self, sig: &'a FnSig, body: &'a Block, own: Option<&'a Ident>) {
        let mut names = vec![];
        if let Some(own) = own.filter(|o| !o.name.is_empty()) {
            self.bind(own, &mut names);
        }
        for p in &sig.params {
            match &p.pattern {
                Some(pat) => self.pattern_names(pat, &mut names),
                None => self.bind(&p.name, &mut names),
            }
        }
        self.scopes.push(names);
        for p in &sig.params {
            if let Some(d) = &p.default {
                self.expr(d);
            }
            if let Some(pat) = &p.pattern {
                self.pattern_defaults(pat);
            }
        }
        self.block(&body.stmts);
        self.scopes.pop();
    }

    fn var(&mut self, v: &'a VarDecl) {
        self.pattern_defaults(&v.pattern);
        if let Some(init) = &v.init {
            self.expr(init);
        }
    }

    /// The names statements `stmts` declare in their block.
    fn block_names(&self, stmts: &'a [Stmt], names: &mut Vec<&'a str>) {
        for s in stmts {
            match &s.kind {
                StmtKind::Var(v) => self.pattern_names(&v.pattern, names),
                StmtKind::Item(item) => self.item_names(item, names),
                _ => {}
            }
        }
    }

    fn block(&mut self, stmts: &'a [Stmt]) {
        let mut names = vec![];
        self.block_names(stmts, &mut names);
        self.scopes.push(names);
        stmts.iter().for_each(|s| self.stmt(s));
        self.scopes.pop();
    }

    fn stmt(&mut self, s: &'a Stmt) {
        match &s.kind {
            StmtKind::Var(v) => self.var(v),
            StmtKind::Expr(e) | StmtKind::Throw(e) | StmtKind::Return(Some(e)) => self.expr(e),
            StmtKind::Return(None)
            | StmtKind::Break(_)
            | StmtKind::Continue(_)
            | StmtKind::Empty => {}
            StmtKind::If { cond, then, els } => {
                self.expr(cond);
                self.block(&then.stmts);
                if let Some(els) = els {
                    self.stmt(els);
                }
            }
            StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
                self.expr(cond);
                self.block(&body.stmts);
            }
            StmtKind::For {
                init,
                cond,
                update,
                body,
            } => {
                let mut names = vec![];
                if let Some(init) = init {
                    self.block_names(std::slice::from_ref(init), &mut names);
                }
                self.scopes.push(names);
                if let Some(init) = init {
                    self.stmt(init);
                }
                cond.iter().for_each(|c| self.expr(c));
                update.iter().for_each(|u| self.expr(u));
                self.block(&body.stmts);
                self.scopes.pop();
            }
            StmtKind::ForOf {
                pattern,
                iter,
                body,
                ..
            } => {
                // The loop variables are in scope (uninitialized) in the iterated expression too.
                let mut names = vec![];
                self.pattern_names(pattern, &mut names);
                self.scopes.push(names);
                self.expr(iter);
                self.pattern_defaults(pattern);
                self.block(&body.stmts);
                self.scopes.pop();
            }
            StmtKind::Block(b) => self.block(&b.stmts),
            StmtKind::Labeled { body, .. } => self.stmt(body),
            StmtKind::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                let mut names = vec![];
                for case in cases {
                    self.block_names(&case.body, &mut names);
                }
                self.scopes.push(names);
                for case in cases {
                    case.test.iter().for_each(|t| self.expr(t));
                    case.body.iter().for_each(|s| self.stmt(s));
                }
                self.scopes.pop();
            }
            StmtKind::Try {
                body,
                catch,
                finally,
            } => {
                self.block(&body.stmts);
                if let Some((param, b)) = catch {
                    let mut names = vec![];
                    if let Some(p) = param {
                        self.pattern_names(p, &mut names);
                    }
                    self.scopes.push(names);
                    if let Some(p) = param {
                        self.pattern_defaults(p);
                    }
                    self.block(&b.stmts);
                    self.scopes.pop();
                }
                if let Some(b) = finally {
                    self.block(&b.stmts);
                }
            }
            StmtKind::Item(item) => self.item(item),
        }
    }

    fn exprs(&mut self, es: &'a [Expr]) {
        es.iter().for_each(|e| self.expr(e));
    }

    fn expr(&mut self, e: &'a Expr) {
        use ExprKind as E;
        match &e.kind {
            E::Ident(id) => self.reference(id),
            E::Lit(_) | E::This | E::Super => {}
            E::Template { exprs, .. } | E::Array(exprs) => self.exprs(exprs),
            E::Unary { expr, .. }
            | E::Update { target: expr, .. }
            | E::Spread(expr)
            | E::Await(expr)
            | E::Cast { expr, .. }
            | E::InstanceOf { expr, .. }
            | E::Paren(expr)
            | E::NonNull(expr)
            | E::Member { object: expr, .. } => self.expr(expr),
            E::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            E::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            E::Cond { cond, then, els } => {
                self.expr(cond);
                self.expr(then);
                self.expr(els);
            }
            E::Call { callee, args, .. } => {
                self.expr(callee);
                self.exprs(args);
            }
            E::New { args, .. } => self.exprs(args),
            E::Yield { arg, .. } => {
                if let Some(a) = arg {
                    self.expr(a);
                }
            }
            E::Index { object, index, .. } => {
                self.expr(object);
                self.expr(index);
            }
            E::Object(props) | E::StructLit { props, .. } => {
                for p in props {
                    match p {
                        ObjectProp::KeyValue(_, x) | ObjectProp::Spread(x) => self.expr(x),
                        ObjectProp::Shorthand(id) => self.reference(id),
                        ObjectProp::Method(f) => self.function(f, None),
                    }
                }
            }
            E::Function(f) => self.function(f, Some(&f.sig.name)),
            E::Arrow { params, body, .. } => {
                let mut names = vec![];
                for p in params {
                    match &p.pattern {
                        Some(pat) => self.pattern_names(pat, &mut names),
                        None => self.bind(&p.name, &mut names),
                    }
                }
                self.scopes.push(names);
                for p in params {
                    if let Some(d) = &p.default {
                        self.expr(d);
                    }
                    if let Some(pat) = &p.pattern {
                        self.pattern_defaults(pat);
                    }
                }
                match body {
                    ArrowBody::Expr(x) => self.expr(x),
                    ArrowBody::Block(b) => self.block(&b.stmts),
                }
                self.scopes.pop();
            }
            E::Jsx(el) => self.jsx(el),
        }
    }

    fn jsx(&mut self, el: &'a JsxElement) {
        for attr in &el.attrs {
            match attr {
                JsxAttr::Spread { expr, .. }
                | JsxAttr::Named {
                    value: Some(JsxAttrValue::Expr { expr, .. }),
                    ..
                } => self.expr(expr),
                JsxAttr::Named {
                    value: Some(JsxAttrValue::Element(inner)),
                    ..
                } => self.jsx(inner),
                JsxAttr::Named { .. } => {}
            }
        }
        for child in &el.children {
            match child {
                JsxChild::Expr {
                    expr: Some(expr), ..
                }
                | JsxChild::Spread { expr, .. } => self.expr(expr),
                JsxChild::Element(inner) => self.jsx(inner),
                JsxChild::Text { .. } | JsxChild::Expr { expr: None, .. } => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::free_values;
    use crate::parse_ts_file;
    use velt_common::FileId;

    /// The byte offsets of the free references to `__DEV__` in `src`.
    fn free(src: &str) -> Vec<u32> {
        let (module, diags) = parse_ts_file(FileId(0), src);
        assert!(diags.is_empty(), "{diags:?}");
        free_values(&module, &|n| n == "__DEV__")
            .iter()
            .map(|id| id.span.lo)
            .collect()
    }

    fn at(src: &str, needle: &str) -> u32 {
        src.find(needle).expect("needle") as u32
    }

    #[test]
    fn declarations_in_scope_hide_the_name() {
        let src = "function f(__DEV__: boolean): boolean { return __DEV__; }\n\
                   function g(): boolean { const __DEV__ = true; return __DEV__; }\n\
                   function h(): void { for (const __DEV__ of [1, 2]) { console.log(__DEV__); } }\n\
                   function k(): void { try { } catch (__DEV__) { console.log(__DEV__); } }\n\
                   function l(): void { for (let __DEV__ = 0; __DEV__ < 1; __DEV__++) {} }\n\
                   const m = (__DEV__: number): number => __DEV__;\n\
                   const n = ({ __DEV__ }: { __DEV__: number }): number => __DEV__;\n\
                   function o(): void { { function __DEV__(): void {} } }\n";
        assert_eq!(free(src), Vec::<u32>::new());
    }

    #[test]
    fn free_uses_are_found_in_every_position() {
        let src = "const a = __DEV__;\nconst o = { __DEV__ };\n\
                   function f(x: boolean = __DEV__): boolean { if (x) { return !__DEV__; } return x; }\n\
                   class C { v: boolean = __DEV__; }\n";
        let lo = |n: usize| {
            src.match_indices("__DEV__")
                .nth(n)
                .map(|(i, _)| i as u32)
                .unwrap()
        };
        assert_eq!(free(src), [lo(0), lo(1), lo(2), lo(3), lo(4)]);
    }

    #[test]
    fn imports_and_top_level_declarations_bind_the_whole_module() {
        let src =
            "import { __DEV__ } from \"./e\";\nexport function f(): boolean { return __DEV__; }\n";
        assert_eq!(free(src), Vec::<u32>::new());
        let src = "export function f(): boolean { return __DEV__; }\nexport function __DEV__(): void {}\n";
        assert_eq!(free(src), Vec::<u32>::new());
        // A sibling scope's declaration does not hide it.
        let src = "function f(): void { const __DEV__ = 1; }\nfunction g(): boolean { return __DEV__; }\n";
        assert_eq!(free(src), [at(src, "__DEV__; }")]);
    }
}
