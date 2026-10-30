//! Top-level statements (a script, as in a TS or JS file): when the root file has statements
//! outside declarations, they run, in order, in a generated `main` (`async` when one of them
//! awaits). A top-level `const`/`let` moves into `main` with them, unless a declaration (a
//! function, class, struct, `extend` block or a module constant that stays) refers to it
//! (`script_names`) or a computed key names it (`[KEY]`, a symbol or string constant): then it
//! stays a module constant, which those declarations can see. So does one that code above it
//! names (a statement, or another variable's initializer): a function that code makes may run
//! after the declaration (`const get = () => K; const K = 1; get();`). And so does one that a
//! function the code makes names (an arrow, a function expression, a method, a function declared
//! in a block; #1872): as in Node, that function sees the module's variable, which it may assign
//! from another task (a request handler), and what the statements assign after making it, where
//! a local of `main` would be captured as a copy. When module state can't hold one that only
//! a function the code makes keeps there (a promise, a variable a `Mutex` `with` callback uses:
//! types the parser can't see), sema checks the program again with it in `main`, where it was
//! before (`velt_sema` `script_locals`). Sema initializes the
//! ones that are module state where they stand among the statements (#1438) and reports a
//! statement that uses a declaration below it directly (`velt_sema` `script_tdz`).
//!
//! The generated function's name has an empty span at the first statement; sema uses that to
//! tell it from a declared function. When the file declares a `main` of its own (`function
//! main() { … }` and then `main();`, as TypeScript programs end, or any other binding named
//! `main`: a variable, a class, an import; #1567), the generated function is named
//! [`crate::SCRIPT_MAIN`] instead, and the declared `main` is an ordinary binding that the
//! statements may use, as in Node.

use super::Parser;
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::{Diagnostic, Severity, Span};

impl Parser<'_> {
    /// Does the cursor start a declaration (as opposed to a statement)?
    pub(super) fn at_item_start(&mut self) -> bool {
        if self.at_decl_word_before_line_break() {
            return false;
        }
        match self.cur_kw() {
            Some(Kw::Import) => !matches!(self.nth(1), Tok::Dot | Tok::LParen),
            Some(
                Kw::Export
                | Kw::Function
                | Kw::Struct
                | Kw::Class
                | Kw::Interface
                | Kw::Enum
                | Kw::Const
                | Kw::Let
                | Kw::Declare,
            ) => true,
            Some(Kw::Async) => self.at_async_function(),
            Some(Kw::Type) => Self::is_ident_like(self.nth(1)),
            None => {
                self.at_extend()
                    || self.at_namespace(0)
                    || self.at_var_decl()
                    || self.at_abstract_class()
            }
            _ => false,
        }
    }

    /// Turn the root file's top-level `stmts` into a generated `main` (see the module docs).
    pub(super) fn finish_script(&mut self, items: &mut Vec<Item>, stmts: Vec<Stmt>) {
        let first = stmts[0].span;
        let has_main = items.iter().any(binds_main);
        if has_main {
            self.warn_unused_main(items, &stmts);
        }
        let keep = self.kept_vars(items, &stmts);
        let mut body = stmts;
        let mut rest = vec![];
        for (item, keep) in std::mem::take(items).into_iter().zip(keep) {
            match item.kind {
                ItemKind::Var(v) if !keep => body.push(Stmt {
                    kind: StmtKind::Var(v),
                    span: item.span,
                }),
                kind => rest.push(Item { kind, ..item }),
            }
        }
        body.sort_by_key(|s| s.span.lo);
        let is_async = self.awaits(&body);
        let last = body.iter().map(|s| s.span.hi).max().unwrap_or(first.hi);
        // From the first variable that moved in, when it stands above the first statement, so
        // the body's span holds all of its code (closures find what they capture by it).
        let lo = body.first().map_or(first.lo, |s| s.span.lo.min(first.lo));
        let span = Span::new(self.file, lo, last);
        let sig = FnSig {
            name: Ident {
                name: if has_main { crate::SCRIPT_MAIN } else { "main" }.into(),
                span: Span::new(self.file, first.lo, first.lo),
            },
            generics: vec![],
            params: vec![],
            ret: None,
            throws: None,
            is_async,
            is_system: false,
            generator: None,
            is_generator: false,
            this_param: None,
            span,
        };
        rest.push(Item {
            kind: ItemKind::Function(FnDecl {
                sig,
                body: Block { stmts: body, span },
                overloads: vec![],
            }),
            exported: false,
            span,
        });
        *items = rest;
    }

    /// A `function main` the statements never call: in a script, `main` is an ordinary function,
    /// which runs only when called (as in Node; a file without statements starts at `main`), so
    /// adding a statement to a program that declares `main` would stop `main` from running.
    fn warn_unused_main(&mut self, items: &[Item], stmts: &[Stmt]) {
        let Some(main) = items.iter().find(|i| {
            !i.exported && matches!(&i.kind, ItemKind::Function(f) if f.sig.name.name == "main")
        }) else {
            return;
        };
        let used = export_listed(items).contains("main")
            || stmts
                .iter()
                .any(|s| super::script_names::stmt_free_names(s).contains("main"))
            || items.iter().any(|i| {
                !std::ptr::eq(i, main) && super::script_names::free_names(i).contains("main")
            });
        if used {
            return;
        }
        let ItemKind::Function(f) = &main.kind else {
            return;
        };
        let mut d = Diagnostic::error(
            "`main` is never called: in a file with top-level statements, the statements are the program and `main` is an ordinary function",
            f.sig.name.span,
        )
        .with_note("call it from the statements (`main();`) to run it, as in Node");
        d.severity = Severity::Warning;
        self.diags.push(d);
    }

    /// Per item: is it a top-level variable that stays at module level? Exported ones do (also by
    /// an export list, `export { x };`), a [`rest_arrow`] constant, a `const` a computed key
    /// `[NAME]` names anywhere in the file (sema resolves the key to the symbol or string the
    /// module constant holds), and those a staying declaration refers to (to a fixed point),
    /// those code above them names (a statement in `stmts` or another variable's initializer)
    /// and those a function the code makes names ([`captured_names`]; module docs).
    fn kept_vars(&self, items: &[Item], stmts: &[Stmt]) -> Vec<bool> {
        let listed = export_listed(items);
        let mut keep: Vec<bool> = items
            .iter()
            .map(|i| match &i.kind {
                ItemKind::Var(v) => {
                    i.exported
                        || bound_names(&v.pattern)
                            .iter()
                            .any(|n| listed.contains(n.as_str()))
                        || rest_arrow(v)
                        || (v.kind == VarKind::Const
                            && bound_names(&v.pattern)
                                .iter()
                                .any(|n| self.computed_keys.contains(n)))
                }
                _ => true,
            })
            .collect();
        let free: Vec<_> = items.iter().map(super::script_names::free_names).collect();
        let stmt_free: Vec<_> = stmts
            .iter()
            .map(|s| (s.span.lo, super::script_names::stmt_free_names(s)))
            .collect();
        let captured = captured_names(items, stmts);
        let assigned = assigned_names(items, stmts);
        for (i, item) in items.iter().enumerate() {
            let ItemKind::Var(v) = &item.kind else {
                continue;
            };
            // A destructuring pattern, a `let` without an initializer and an awaiting initializer
            // cannot stay at module level (yet): such a variable stays in `main`.
            let simple = matches!(v.pattern.kind, PatternKind::Ident(_))
                && v.init
                    .as_ref()
                    .is_some_and(|e| !self.mentions(e.span, "await"));
            if !simple {
                continue;
            }
            let names = bound_names(&v.pattern);
            let own_captured = v.init.as_ref().map(own_captures).unwrap_or_default();
            let named =
                |free: &rustc_hash::FxHashSet<String>| names.iter().any(|n| free.contains(n));
            let above = stmt_free
                .iter()
                .any(|(lo, free)| *lo < item.span.lo && named(free))
                || items.iter().zip(&free).any(|(j, free)| {
                    matches!(j.kind, ItemKind::Var(_)) && j.span.lo < item.span.lo && named(free)
                });
            // A closure (a function the code makes) refers to the variable itself, as in Node: it
            // sees what the code assigns after making it, and it may assign the variable, or
            // change the new object it holds (`new Map()`, `[]`), on another task (a request
            // handler; #1872). A copy of a variable nothing assigns, holding no such object, is
            // the variable, so that one stays a local of `main`. So does a new object a closure
            // in its own initializer names (`const p = new Proxy(t, { get: () => p })`, which
            // would need a type as module state), and one whose declared type is wider than the
            // object (`const m: object = new Map()`): the checks the statements make narrow a
            // local (`m instanceof Map`), not module state. A new promise isn't such an object
            // (`fresh_object`); a `let` that holds one is moved back into `main` by sema.
            let fresh = v.init.as_ref().is_some_and(fresh_object)
                && !v.ty.as_ref().is_some_and(wide_type)
                && !own_captured.iter().any(|n| names.contains(n));
            let changes =
                fresh || (v.kind == VarKind::Let && names.iter().any(|n| assigned.contains(n)));
            let by_closure = changes && captured.iter().any(|n| names.contains(n));
            keep[i] |= above || by_closure;
        }
        loop {
            let mut changed = false;
            for (i, item) in items.iter().enumerate() {
                let ItemKind::Var(v) = &item.kind else {
                    continue;
                };
                if keep[i] {
                    continue;
                }
                let names = bound_names(&v.pattern);
                let used = (0..items.len())
                    .any(|j| j != i && keep[j] && names.iter().any(|n| free[j].contains(n)));
                if used {
                    keep[i] = true;
                    changed = true;
                }
            }
            if !changed {
                return keep;
            }
        }
    }

    /// Does one of `stmts` await at their own level (`await`, `for await`, `await using`), not
    /// only in a function they make (an arrow, a function expression, a method, a nested
    /// declaration), which may run later (#1859)?
    fn awaits(&self, stmts: &[Stmt]) -> bool {
        struct Nested(Vec<Span>);
        impl<'a> crate::visit::Visit<'a> for Nested {
            fn item(&mut self, item: &'a Item) {
                self.0.push(item.span);
            }
            fn function(&mut self, _sig: &'a FnSig, body: &'a Block) {
                self.0.push(body.span);
            }
            fn expr(&mut self, e: &'a Expr) {
                if matches!(e.kind, ExprKind::Arrow { .. } | ExprKind::Function(_)) {
                    self.0.push(e.span);
                }
            }
        }
        let mut nested = Nested(vec![]);
        for s in stmts {
            crate::visit::walk_stmt(s, &mut nested);
        }
        let inside = |at: u32| nested.0.iter().any(|n| n.lo <= at && at < n.hi);
        stmts.iter().any(|s| {
            self.mention_offsets(s.span, "await")
                .any(|at| !inside(s.span.lo + at as u32))
        })
    }

    /// Does the source text in `span` contain `word` as a whole identifier, not after a `.`
    /// (`x.word` is a member, not a reference)?
    fn mentions(&self, span: Span, word: &str) -> bool {
        self.mention_offsets(span, word).next().is_some()
    }

    /// Where `word` stands as a word in the text of `span`, relative to its start.
    fn mention_offsets<'s>(
        &'s self,
        span: Span,
        word: &'s str,
    ) -> impl Iterator<Item = usize> + 's {
        let text = &self.src[span.lo as usize..span.hi as usize];
        let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
        text.match_indices(word)
            .filter(move |(at, _)| {
                let before = text[..*at].trim_end().chars().next_back();
                let touching = text[..*at].chars().next_back().is_some_and(ident);
                let after = text[at + word.len()..].chars().next();
                !touching && before != Some('.') && !after.is_some_and(ident)
            })
            .map(|(at, _)| at)
    }
}

/// The top-level names that the functions the code in `main` makes (an arrow, a function
/// expression, a method, a nested declaration, in a statement or in the initializer of a
/// variable) refer to.
fn captured_names(items: &[Item], stmts: &[Stmt]) -> rustc_hash::FxHashSet<String> {
    let inits = items.iter().filter_map(|i| match &i.kind {
        ItemKind::Var(v) => v.init.as_ref(),
        _ => None,
    });
    stmts
        .iter()
        .flat_map(super::script_names::stmt_free_uses)
        .chain(inits.flat_map(super::script_names::expr_free_uses))
        .filter(|u| u.deferred)
        .map(|u| u.name)
        .collect()
}

/// The names the code in `main` assigns (`x = …`, `x += …`, `x++`, `[x, y] = …`), also in the
/// functions it makes. A name a nested scope declares counts too: it only keeps a variable at
/// module level.
fn assigned_names(items: &[Item], stmts: &[Stmt]) -> rustc_hash::FxHashSet<String> {
    struct Assigned(rustc_hash::FxHashSet<String>);
    impl Assigned {
        fn target(&mut self, e: &Expr) {
            match &e.kind {
                ExprKind::Ident(id) => {
                    self.0.insert(id.name.clone());
                }
                ExprKind::Paren(e) | ExprKind::NonNull(e) | ExprKind::Cast { expr: e, .. } => {
                    self.target(e)
                }
                ExprKind::Array(es) => es.iter().for_each(|e| self.target(e)),
                ExprKind::Spread(e) => self.target(e),
                ExprKind::Assign { target, .. } => self.target(target),
                ExprKind::Object(props) => {
                    for p in props {
                        match p {
                            ObjectProp::KeyValue(_, e) | ObjectProp::Spread(e) => self.target(e),
                            ObjectProp::Shorthand(id) => {
                                self.0.insert(id.name.clone());
                            }
                            ObjectProp::Method(_) => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    impl<'a> crate::visit::Visit<'a> for Assigned {
        fn expr(&mut self, e: &'a Expr) {
            if let ExprKind::Assign { target, .. } | ExprKind::Update { target, .. } = &e.kind {
                self.target(target);
            }
        }
    }
    let mut a = Assigned(rustc_hash::FxHashSet::default());
    for s in stmts {
        crate::visit::walk_stmt(s, &mut a);
    }
    for i in items {
        if let ItemKind::Var(VarDecl { init: Some(e), .. }) = &i.kind {
            crate::visit::walk_expr(e, &mut a);
        }
    }
    a.0
}

/// Does `e` make a new object the code may change: `new C(…)` or an array literal? (Not an
/// object literal yet: sema checks some uses of one, such as `Object.defineProperty` on it, only
/// on a local. Not `new Promise(…)`, which module state cannot hold and nothing changes: a
/// closure awaits the same promise.)
fn fresh_object(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::New { class, .. } => !matches!(&class.kind,
            TypeExprKind::Named { path, .. } if matches!(path.as_slice(), [c] if c.name == "Promise")),
        ExprKind::Array(_) => true,
        ExprKind::Paren(e) | ExprKind::Cast { expr: e, .. } => fresh_object(e),
        _ => false,
    }
}

/// The names the functions that `init` makes refer to.
fn own_captures(init: &Expr) -> rustc_hash::FxHashSet<String> {
    super::script_names::expr_free_uses(init)
        .into_iter()
        .filter(|u| u.deferred)
        .map(|u| u.name)
        .collect()
}

/// A declared type that checks narrow: a union, `object`, `unknown` or `any`.
fn wide_type(t: &TypeExpr) -> bool {
    match &t.kind {
        TypeExprKind::Union(_) => true,
        TypeExprKind::Named { path, .. } => {
            matches!(path.as_slice(), [n] if matches!(n.name.as_str(), "object" | "Object" | "unknown" | "any"))
        }
        _ => false,
    }
}

/// The local names an export list (`export { a, b as c };`) exports: the variables among them
/// stay at module level, as an exported variable does.
fn export_listed(items: &[Item]) -> rustc_hash::FxHashSet<&str> {
    items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Import(imp) if imp.from.is_empty() => Some(imp),
            _ => None,
        })
        .flat_map(|imp| imp.names.iter().map(|n| n.name.name.as_str()))
        .collect()
}

/// Does the item declare something named `main` (a function, a variable, a class, an import, …)?
/// Then the generated function can't be named `main` too (#1567).
fn binds_main(item: &Item) -> bool {
    let is_main = |i: &Ident| i.name == "main";
    match &item.kind {
        ItemKind::Function(f) => is_main(&f.sig.name),
        ItemKind::ExternFn(sig) => is_main(&sig.name),
        ItemKind::Struct(t) | ItemKind::Class(t) => is_main(&t.name),
        ItemKind::Interface(i) => is_main(&i.name),
        ItemKind::Enum(e) => is_main(&e.name),
        ItemKind::TypeAlias(t) => is_main(&t.name),
        ItemKind::Namespace(n) => is_main(&n.name),
        ItemKind::Var(v) => bound_names(&v.pattern).iter().any(|n| n == "main"),
        // An export list (`export { main };`, empty `from`) binds nothing.
        ItemKind::Import(i) => {
            !i.from.is_empty()
                && (i.namespace.as_ref().is_some_and(is_main)
                    || i.names
                        .iter()
                        .any(|n| is_main(n.alias.as_ref().unwrap_or(&n.name))))
        }
        ItemKind::Extend(_) | ItemKind::Augment(_) => false,
    }
}

/// `const f = (...xs: T[]) => …` (or a function expression): a non-generic arrow constant with
/// a rest parameter and typed parameters, which sema checks as the function `function f(...xs:
/// T[])` at module level (a function value cannot take rest arguments yet), so it stays there
/// like a function declaration.
fn rest_arrow(v: &VarDecl) -> bool {
    let Some(Expr {
        kind:
            ExprKind::Arrow {
                type_params,
                params,
                ..
            },
        ..
    }) = &v.init
    else {
        return false;
    };
    v.kind == VarKind::Const
        && v.ty.is_none()
        && matches!(v.pattern.kind, PatternKind::Ident(_))
        && type_params.is_empty()
        && params.iter().any(|p| p.rest)
        && params.iter().all(|p| p.ty.is_some())
}

/// The names a declaration pattern binds.
fn bound_names(p: &Pattern) -> Vec<String> {
    match &p.kind {
        PatternKind::Ident(i) => vec![i.name.clone()],
        PatternKind::Wildcard => vec![],
        PatternKind::Object { fields, rest } => fields
            .iter()
            .flat_map(|(_, p)| bound_names(p))
            .chain(rest.iter().map(|r| r.name.clone()))
            .collect(),
        PatternKind::Array { elems, rest } => elems
            .iter()
            .flat_map(bound_names)
            .chain(rest.iter().map(|r| r.name.clone()))
            .collect(),
        PatternKind::Default { pattern, .. } => bound_names(pattern),
    }
}
