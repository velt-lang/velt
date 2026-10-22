//! A top-level statement that uses a class, an enum or a top-level `let`/`const` declared after
//! it, directly rather than in a function it makes: in JavaScript the declaration has not been
//! evaluated yet when the statement runs, and the use throws a `ReferenceError` (TypeScript
//! rejects it: "used before its declaration"). Velt evaluates such a declaration where it
//! stands too (#1438), but a class or a constant whose value needs no evaluation would work
//! early, so the use is an error. A use inside a function or arrow runs when it is called and
//! is not checked here (a slot read before its initializer stops with the `ReferenceError` at
//! run time).

use rustc_hash::FxHashSet;

use crate::ast::*;
use crate::visit::{walk_expr, walk_stmt, Visit};
use velt_common::Span;

/// The direct uses of the top-level declarations of `items` in the code before them that runs
/// at once: `stmts`, the initializers of `items`' variables, and their classes' `extends`
/// clauses and static field initializers. Each is (name, what it is, where it is used).
pub(super) fn early_uses(items: &[Item], stmts: &[Stmt]) -> Vec<(String, &'static str, Span)> {
    let mut decls: Vec<(String, &'static str, u32)> = vec![];
    for item in items {
        match &item.kind {
            ItemKind::Class(d) => decls.push((d.name.name.clone(), "class", item.span.lo)),
            ItemKind::Enum(e) => decls.push((e.name.name.clone(), "enum", item.span.lo)),
            ItemKind::Var(v) => {
                let mut names = vec![];
                pattern_names(&v.pattern, &mut names);
                decls.extend(names.into_iter().map(|n| (n, "variable", item.span.lo)));
            }
            _ => {}
        }
    }
    let mut out = vec![];
    let Some(last) = decls.iter().map(|(_, _, lo)| *lo).max() else {
        return out;
    };
    let mut check = |lo: u32, walk: &dyn Fn(&mut Uses)| {
        if lo >= last {
            return;
        }
        let mut uses = Uses::default();
        walk(&mut uses);
        for (name, at) in &uses.uses {
            let later = uses.deferred.iter().any(|d| d.lo <= at.lo && at.hi <= d.hi);
            if later || uses.binds.contains(name) {
                continue;
            }
            if let Some((_, what, _)) = decls.iter().find(|(n, _, at)| n == name && *at > lo) {
                out.push((name.clone(), *what, *at));
            }
        }
    };
    for s in stmts {
        check(s.span.lo, &|u| walk_stmt(s, u));
    }
    for item in items {
        match &item.kind {
            ItemKind::Var(v) => {
                if let Some(init) = &v.init {
                    check(item.span.lo, &|u| walk_expr(init, u));
                }
            }
            ItemKind::Class(d) => {
                if let Some(TypeExpr {
                    kind: TypeExprKind::Named { path, .. },
                    ..
                }) = &d.extends
                {
                    if let [head] = path.as_slice() {
                        check(item.span.lo, &|u| {
                            u.uses.push((head.name.clone(), head.span))
                        });
                    }
                }
                for f in d.fields.iter().filter(|f| f.is_static) {
                    if let Some(e) = &f.default {
                        check(item.span.lo, &|u| walk_expr(e, u));
                    }
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|(_, _, at)| at.lo);
    out
}

/// What one statement uses directly, binds, and runs later (function bodies).
#[derive(Default)]
struct Uses {
    uses: Vec<(String, Span)>,
    /// Any name the statement binds anywhere (a block's local may shadow the declaration).
    binds: FxHashSet<String>,
    /// Code that runs only when called: the bodies of functions and arrows, nested
    /// declarations.
    deferred: Vec<Span>,
}

impl<'a> Visit<'a> for Uses {
    fn item(&mut self, item: &'a Item) {
        self.deferred.push(item.span);
        match &item.kind {
            ItemKind::Class(d) | ItemKind::Struct(d) => {
                self.binds.insert(d.name.name.clone());
            }
            ItemKind::Function(f) => {
                self.binds.insert(f.sig.name.name.clone());
            }
            ItemKind::Enum(e) => {
                self.binds.insert(e.name.name.clone());
            }
            _ => {}
        }
    }

    fn function(&mut self, _sig: &'a FnSig, body: &'a Block) {
        self.deferred.push(body.span);
    }

    fn stmt(&mut self, s: &'a Stmt) {
        match &s.kind {
            StmtKind::ForOf { pattern, .. } => self.bind(pattern),
            StmtKind::Try {
                catch: Some((Some(p), _)),
                ..
            } => self.bind(p),
            _ => {}
        }
    }

    fn var_decl(&mut self, v: &'a VarDecl) {
        self.bind(&v.pattern);
    }

    fn expr(&mut self, e: &'a Expr) {
        match &e.kind {
            ExprKind::Ident(id) => self.uses.push((id.name.clone(), id.span)),
            ExprKind::New { class, .. } => {
                if let TypeExprKind::Named { path, .. } = &class.kind {
                    if let [head] = path.as_slice() {
                        self.uses.push((head.name.clone(), head.span));
                    }
                }
            }
            ExprKind::Object(props) => {
                for p in props {
                    if let ObjectProp::Shorthand(id) = p {
                        self.uses.push((id.name.clone(), id.span));
                    }
                }
            }
            ExprKind::Arrow { .. } | ExprKind::Function(_) => self.deferred.push(e.span),
            _ => {}
        }
    }
}

impl Uses {
    fn bind(&mut self, p: &Pattern) {
        let mut names = vec![];
        pattern_names(p, &mut names);
        self.binds.extend(names);
    }
}

fn pattern_names(p: &Pattern, out: &mut Vec<String>) {
    match &p.kind {
        PatternKind::Ident(i) => out.push(i.name.clone()),
        PatternKind::Wildcard => {}
        PatternKind::Object { fields, rest } => {
            fields.iter().for_each(|(_, p)| pattern_names(p, out));
            out.extend(rest.iter().map(|r| r.name.clone()));
        }
        PatternKind::Array { elems, rest } => {
            elems.iter().for_each(|p| pattern_names(p, out));
            out.extend(rest.iter().map(|r| r.name.clone()));
        }
        PatternKind::Default { pattern, .. } => pattern_names(pattern, out),
    }
}

#[cfg(test)]
mod tests {
    use crate::parse_file;
    use velt_common::FileId;

    /// The "used before its declaration" errors of `src`.
    fn early(src: &str) -> Vec<String> {
        let (_, diags) = parse_file(FileId(0), src);
        diags
            .iter()
            .filter(|d| d.message.contains("before its declaration"))
            .map(|d| d.message.clone())
            .collect()
    }

    #[test]
    fn direct_uses_before_a_declaration_are_errors() {
        assert_eq!(
            early("console.log(K);\nconst K = 1;\nfunction f(): number { return K; }\n"),
            ["`K` is used before its declaration"]
        );
        assert_eq!(
            early("const o = { K };\nconsole.log(o);\nconst K = 1;\n"),
            ["`K` is used before its declaration"]
        );
        assert_eq!(
            early("if (true) { new C(); }\nclass C {}\n"),
            ["class `C` is used before its declaration"]
        );
    }

    #[test]
    fn deferred_and_shadowed_uses_are_not() {
        let src = "const f = (): number => K;\n\
                   const g = function (): number { return K; };\n\
                   const o = { m(): number { return K; } };\n\
                   { const K = 2; console.log(K); }\n\
                   for (const C of [1]) { console.log(C); }\n\
                   const K = 1;\n\
                   class C { v: number = K; static w: number = K; }\n\
                   console.log(f(), g(), o.m(), K);\n";
        assert_eq!(early(src), Vec::<String>::new());
    }
}
