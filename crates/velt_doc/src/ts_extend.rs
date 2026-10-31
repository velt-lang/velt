//! Extensions (`extend<T> Target<…> { … }`) as TypeScript: methods added to the target's
//! interface ([`TsExtension`]).

use velt_syntax::ast::{self, TypeExprKind};

use crate::ts::{indented, mentions, TsExtMember, TsExtension, TsPrinter, FIRST_PARAM, NUMBERS};

impl TsPrinter<'_> {
    /// `e` as a [`TsExtension`]; `None` when TypeScript cannot add methods to its target (a
    /// union such as `T | null`, a type parameter: `extend<T> T`).
    pub(crate) fn extension(&self, e: &ast::ExtendDecl) -> Option<TsExtension> {
        let TypeExprKind::Named { path, args } = &e.target.kind else {
            return None;
        };
        let name = path
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        if e.generics.iter().any(|g| g.name.name == name) {
            return None;
        }
        let target = match name.as_str() {
            "string" => "String".to_string(),
            "bool" | "boolean" => "Boolean".to_string(),
            n if NUMBERS.contains(&n) => "Number".to_string(),
            _ => name,
        };
        let mark = self.enter(&e.generics);
        // The target as a type (`Array<T[]>`, `string`), for the `this` parameters.
        let this_ty = self.ty(&e.target);
        let args: Vec<String> = match self.arity(&target) {
            Some(n) => args.iter().take(n).map(|a| self.ty(a)).collect(),
            None => args.iter().map(|a| self.ty(a)).collect(),
        };
        let mut members = vec![];
        for m in e.methods.iter().filter(|m| !m.is_private && !m.is_static) {
            let doc = self.doc_comment(m.decl.sig.span.lo);
            let this_form = match (m.is_getter, m.is_setter) {
                (_, true) => None,
                (true, _) => self
                    .getter_this_form(e, args.first(), m)
                    .map(|t| indented(&doc, &t)),
                _ => {
                    let sigs = match m.decl.overloads.is_empty() {
                        true => std::slice::from_ref(&m.decl.sig),
                        false => &m.decl.overloads[..],
                    };
                    let lines: Vec<String> = sigs
                        .iter()
                        .map(|s| format!("{};", self.fn_sig_this(s, &e.generics, Some(&this_ty))))
                        .collect();
                    Some(indented(&doc, &lines.join("\n  ")))
                }
            };
            members.push(TsExtMember {
                accessor: m.is_getter || m.is_setter,
                direct: self.method(m),
                this_form,
            });
        }
        self.scope.borrow_mut().truncate(mark);
        self.refer(&target, 0);
        Some(TsExtension {
            target,
            args,
            members,
        })
    }

    /// A getter of an extension of a one-parameter target in the `this` form: `readonly name:
    /// P0 extends Arg<infer T> ? R : never;` (the extension's type parameters the argument does
    /// not show are `unknown`).
    fn getter_this_form(
        &self,
        e: &ast::ExtendDecl,
        arg: Option<&String>,
        m: &ast::Method,
    ) -> Option<String> {
        let TypeExprKind::Named { args, .. } = &e.target.kind else {
            return None;
        };
        let [pattern] = &args[..] else {
            return None;
        };
        let names: Vec<String> = e.generics.iter().map(|g| g.name.name.clone()).collect();
        // Which type parameters the argument shows, as TypeScript prints it.
        let shown = arg?;
        let in_pattern: Vec<&String> = names.iter().filter(|n| mentions(shown, n)).collect();
        for n in &names {
            self.subst
                .borrow_mut()
                .insert(n.clone(), format!("infer {n}"));
        }
        let pattern = self.ty(pattern);
        for n in &names {
            let to = match in_pattern.contains(&n) {
                true => n.clone(),
                false => "unknown".into(),
            };
            self.subst.borrow_mut().insert(n.clone(), to);
        }
        let ret = m.decl.sig.ret.as_ref().map(|r| self.ty(r));
        self.subst.borrow_mut().clear();
        Some(format!(
            "readonly {}: {FIRST_PARAM} extends {pattern} ? {} : never;",
            m.decl.sig.name.name,
            ret.unwrap_or_else(|| "void".into())
        ))
    }

    /// How many type arguments TypeScript's `name` takes, when Velt's has more (the error type
    /// of promises and iterators).
    fn arity(&self, name: &str) -> Option<usize> {
        matches!(
            name,
            "Promise"
                | "Iterator"
                | "Iterable"
                | "AsyncIterator"
                | "AsyncIterable"
                | "IterableIterator"
                | "IteratorObject"
                | "AsyncIterableIterator"
                | "Generator"
                | "AsyncGenerator"
        )
        .then_some(1)
    }
}
