//! Recursive type aliases of object types (#654): `type Tree = { kids: Tree[] } & { v: number }`
//! and `type Tree = { kids: Tree[]; v: number }`. An alias is expanded where it is used, so one
//! that names itself would expand without end; an interface is a definition that its own fields
//! can name. Such an alias, whose body is an object type or an intersection of object types,
//! is checked as the field-only interface with the same fields in the same order
//! (`collect::field_only`): an object type like the alias's, with the same layout and printing,
//! the same name in messages, and the same generics. The syntax is rewritten before collection,
//! like `generic_arrows`.
//!
//! - A part may name a non-generic alias of such a type, or a field-only interface, declared in
//!   the same module (`type Tree = Base & { kids: Tree[] }`, #783): its fields stand where its
//!   name does. A field in two parts must have the same type in both.
//! - Aliases naming each other (`type A = { b: B | null }; type B = { a: A[] }`) are rewritten
//!   together: every object alias on such a cycle ([`cyclic_aliases`]) whose fields name one of
//!   the cycle becomes an interface.
//!
//! Any other alias naming itself stays an error (`resolve::expand_alias`, [`self_reference`]);
//! for one on a cycle, the error names the part that kept it from being rewritten.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::SourceModule;

/// Why an alias on a cycle was not rewritten, by the span of its name: the part (or field) that
/// kept it from being rewritten, and what is wrong with it.
pub(crate) type Blocked = FxHashMap<Span, (Span, String)>;

/// The modules with their recursive object type aliases rewritten into interfaces (`None` when
/// no module has one: the common case, nothing is copied), and why the others were not.
pub(crate) fn rewrite(modules: &[SourceModule]) -> (Option<Vec<SourceModule>>, Blocked) {
    let mut blocked = Blocked::default();
    let rewritten: Vec<Vec<Option<ast::Item>>> = modules
        .iter()
        .map(|m| {
            let mut x = Expander::new(&m.ast.items);
            let out = m.ast.items.iter().map(|i| x.as_interface(i)).collect();
            blocked.extend(x.blocked);
            out
        })
        .collect();
    if rewritten.iter().flatten().all(Option::is_none) {
        return (None, blocked);
    }
    let modules = modules
        .iter()
        .zip(rewritten)
        .map(|(m, new)| SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            file: m.file,
            ast: ast::Module {
                items: (m.ast.items.iter().zip(new))
                    .map(|(i, n)| n.unwrap_or_else(|| i.clone()))
                    .collect(),
                span: m.ast.span,
                jsx_import_source: m.ast.jsx_import_source.clone(),
            },
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        })
        .collect();
    (Some(modules), blocked)
}

/// The error for a use of alias `name` while it is being expanded: it refers to itself, with
/// what kept it from being rewritten into an interface when the rewrite says so.
pub(crate) fn self_reference(blocked: &Blocked, name: &ast::Ident, at: Span) -> Diagnostic {
    let d = Diagnostic::error(format!("type alias `{}` refers to itself", name.name), at);
    match blocked.get(&name.span) {
        Some((part, why)) => d.with_label(*part, why.clone()).with_note(format!(
            "`{}` would be checked as an interface, but {why}; parts must be object types, non-generic aliases of them or field-only interfaces declared in the same module, with no field of two types",
            name.name
        )),
        None => d.with_note("an alias may refer to itself when it is an object type or an intersection of object types (written out, or non-generic aliases of them or field-only interfaces declared in the same module) with no field of two types, or a union that names itself behind an array, an index signature, a `Map` or a function type (`type Json = string | Json[]`); give any other recursive type a nominal member, such as an interface or a class"),
    }
}

/// A type name a part may refer to.
#[derive(Clone, Copy)]
enum Decl<'a> {
    Alias(&'a ast::TypeAlias),
    Iface(&'a ast::InterfaceDecl),
}

/// Expands one module's recursive object type aliases into their fields. Only aliases on a cycle
/// are expanded, and one expansion visits each named part at most once (naming it again names
/// the same fields again), so aliases naming each other many times (`type A0 = A1 & A1`) cost
/// their number, not the number of paths through them.
struct Expander<'a> {
    /// The module's type aliases and interfaces, by name.
    decls: FxHashMap<&'a str, Decl<'a>>,
    /// The cycle each alias that may be rewritten lies on ([`cyclic_aliases`]).
    cyclic: FxHashMap<&'a str, usize>,
    /// The alias being rewritten: its type parameters, the named parts being expanded, those
    /// expanded already, and its fields so far.
    generics: Vec<&'a str>,
    seen: Vec<&'a str>,
    visited: FxHashSet<&'a str>,
    fields: Vec<ast::Field>,
    field_index: FxHashMap<String, usize>,
    /// Why aliases on a cycle were not rewritten ([`Blocked`]).
    blocked: Blocked,
    /// Parts visited, over all expansions (the unit tests' measure of work).
    steps: u64,
}

impl<'a> Expander<'a> {
    fn new(items: &'a [ast::Item]) -> Self {
        let mut decls = FxHashMap::default();
        for i in items {
            let (name, d) = match &i.kind {
                ast::ItemKind::TypeAlias(a) => (&a.name.name, Decl::Alias(a)),
                ast::ItemKind::Interface(f) => (&f.name.name, Decl::Iface(f)),
                _ => continue,
            };
            decls.entry(name.as_str()).or_insert(d);
        }
        Expander {
            decls,
            cyclic: cyclic_aliases(items),
            generics: vec![],
            seen: vec![],
            visited: FxHashSet::default(),
            fields: vec![],
            field_index: FxHashMap::default(),
            blocked: Blocked::default(),
            steps: 0,
        }
    }

    /// `item` as an interface, if it is an alias of object types on a cycle whose fields name
    /// an alias of that cycle; for one that can't be rewritten, the reason in `blocked`.
    fn as_interface(&mut self, item: &'a ast::Item) -> Option<ast::Item> {
        let ast::ItemKind::TypeAlias(a) = &item.kind else {
            return None;
        };
        let &cycle = self.cyclic.get(a.name.name.as_str())?;
        if !matches!(
            a.ty.kind,
            ast::TypeExprKind::Intersection(_) | ast::TypeExprKind::Object(_)
        ) {
            return None;
        }
        self.generics = a.generics.iter().map(|g| g.name.name.as_str()).collect();
        self.seen = vec![a.name.name.as_str()];
        self.visited.clear();
        self.fields.clear();
        self.field_index.clear();
        if let Err(why) = self.object_parts(&a.ty) {
            self.blocked.insert(a.name.span, why);
            return None;
        }
        let fields = std::mem::take(&mut self.fields);
        let in_cycle = |n: &str| self.cyclic.get(n) == Some(&cycle);
        if fields.is_empty() || !fields.iter().any(|f| any_name(&f.ty, &mut |n| in_cycle(n))) {
            return None;
        }
        let decl = ast::InterfaceDecl {
            name: a.name.clone(),
            generics: a.generics.clone(),
            extends: vec![],
            fields,
            methods: vec![],
            call_sigs: vec![],
        };
        Some(ast::Item {
            kind: ast::ItemKind::Interface(decl),
            exported: item.exported,
            span: item.span,
        })
    }

    /// Appends the fields of part `t` to `self.fields`: an object type, an intersection of
    /// them, or the name of a non-generic alias of one or of a field-only interface in the same
    /// module. `Err` with the part (or field) that can't be, and why. Walks with a stack of its
    /// own, as a chain of aliases may be long.
    fn object_parts(&mut self, t: &'a ast::TypeExpr) -> Result<(), (Span, String)> {
        enum Work<'a> {
            Part(&'a ast::TypeExpr),
            /// Done with the named part being expanded.
            Leave,
        }
        let mut work = vec![Work::Part(t)];
        while let Some(w) = work.pop() {
            let t = match w {
                Work::Part(t) => t,
                Work::Leave => {
                    self.seen.pop();
                    continue;
                }
            };
            self.steps += 1;
            let name = match &t.kind {
                ast::TypeExprKind::Object(fs) => {
                    fs.iter().try_for_each(|f| self.add(interface_field(f)))?;
                    continue;
                }
                ast::TypeExprKind::Intersection(ps) => {
                    work.extend(ps.iter().rev().map(Work::Part));
                    continue;
                }
                ast::TypeExprKind::Named { path, args } if path.len() == 1 && args.is_empty() => {
                    path[0].name.as_str()
                }
                ast::TypeExprKind::Named { .. } => {
                    let why = "this part is a generic type or a type of another module";
                    return Err((t.span, why.into()));
                }
                _ => return Err((t.span, "this part is not an object type".into())),
            };
            let fail = |why: String| Err((t.span, why));
            if self.generics.contains(&name) {
                return fail(format!("`{name}` is a type parameter"));
            }
            if self.seen.contains(&name) {
                return fail(format!("`{name}` is part of itself"));
            }
            // Named again: the same fields again.
            if !self.visited.insert(name) {
                continue;
            }
            match self.decls.get(name).copied() {
                Some(Decl::Alias(a)) if a.generics.is_empty() => {
                    self.seen.push(name);
                    work.push(Work::Leave);
                    work.push(Work::Part(&a.ty));
                }
                Some(Decl::Alias(_)) => return fail(format!("`{name}` is a generic alias")),
                Some(Decl::Iface(i))
                    if i.generics.is_empty()
                        && i.extends.is_empty()
                        && i.methods.is_empty()
                        && i.call_sigs.is_empty() =>
                {
                    i.fields.iter().try_for_each(|f| self.add(f.clone()))?;
                }
                Some(Decl::Iface(_)) => {
                    return fail(format!(
                        "`{name}` is an interface with type parameters, methods, call signatures or `extends`"
                    ));
                }
                None => {
                    return fail(format!(
                        "`{name}` is not an alias or interface declared in this module"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Adds field `f`; one with its name already there must be the same field (TypeScript
    /// merges them; with different types they would be an intersection, which a field can't be).
    fn add(&mut self, f: ast::Field) -> Result<(), (Span, String)> {
        match self.field_index.get(&f.name.name).map(|&i| &self.fields[i]) {
            None => {
                self.field_index
                    .insert(f.name.name.clone(), self.fields.len());
                self.fields.push(f);
                Ok(())
            }
            Some(g)
                if g.optional == f.optional
                    && g.readonly == f.readonly
                    && type_key(&g.ty).is_some_and(|k| Some(k) == type_key(&f.ty)) =>
            {
                Ok(())
            }
            Some(_) => Err((
                f.span,
                format!("field `{}` has another type in another part", f.name.name),
            )),
        }
    }
}

/// `t` written without spans, to compare two type expressions; `None` for kinds it doesn't
/// cover (never equal).
fn type_key(t: &ast::TypeExpr) -> Option<String> {
    use ast::TypeExprKind as K;
    let all = |ts: &[ast::TypeExpr], sep: &str| -> Option<String> {
        let ks: Option<Vec<String>> = ts.iter().map(type_key).collect();
        Some(ks?.join(sep))
    };
    Some(match &t.kind {
        K::Named { path, args } => {
            let p: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
            format!("{}<{}>", p.join("."), all(args, ",")?)
        }
        K::Array(e) => format!("{}[]", type_key(e)?),
        K::Tuple(ts) => format!("[{}]", all(ts, ",")?),
        K::Union(ts) => format!("({})", all(ts, "|")?),
        K::Intersection(ts) => format!("({})", all(ts, "&")?),
        K::Literal(l) => format!("{l:?}"),
        K::Null => "null".into(),
        K::Undefined => "undefined".into(),
        K::Void => "void".into(),
        K::Object(fs) => {
            let ks: Option<Vec<String>> = fs
                .iter()
                .map(|f| {
                    let k = type_key(&f.ty)?;
                    Some(format!("{}{}{}:{k}", f.readonly, f.name.name, f.optional))
                })
                .collect();
            format!("{{{}}}", ks?.join(";"))
        }
        _ => return None,
    })
}

/// The type aliases among `items` on a cycle of aliases naming each other (or themselves), by
/// the cycle: Tarjan's strongly connected components, without recursion, in time linear in the
/// aliases and the names they mention (an alias's own type parameters are not names of
/// aliases). Only cycles of aliases that are object types, intersections or names of other
/// types count: one through a union (`type Json = string | Json[]`) is
/// `recursive_unions`'s, and stays so.
fn cyclic_aliases(items: &[ast::Item]) -> FxHashMap<&str, usize> {
    let aliases: Vec<&ast::TypeAlias> = items
        .iter()
        .filter_map(|i| match &i.kind {
            ast::ItemKind::TypeAlias(a) => Some(a),
            _ => None,
        })
        .collect();
    let index: FxHashMap<&str, usize> = aliases
        .iter()
        .enumerate()
        .map(|(i, a)| (a.name.name.as_str(), i))
        .collect();
    let edges: Vec<Vec<usize>> = aliases
        .iter()
        .map(|a| {
            let mut out = vec![];
            any_name(&a.ty, &mut |n| {
                if !a.generics.iter().any(|g| g.name.name == n) {
                    out.extend(index.get(n));
                }
                false
            });
            out
        })
        .collect();
    let object = |a: &ast::TypeAlias| match &a.ty.kind {
        ast::TypeExprKind::Object(_) | ast::TypeExprKind::Intersection(_) => true,
        ast::TypeExprKind::Named { path, args } => path.len() == 1 && args.is_empty(),
        _ => false,
    };
    const NONE: usize = usize::MAX;
    let n = aliases.len();
    let (mut order, mut low, mut on_stack) = (vec![NONE; n], vec![0; n], vec![false; n]);
    let (mut stack, mut next, mut cyclic) = (vec![], 0, FxHashMap::default());
    for root in 0..n {
        if order[root] != NONE {
            continue;
        }
        // (node, index of its next edge to follow)
        let mut work = vec![(root, 0)];
        order[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (v, ref mut e)) = work.last_mut() {
            if let Some(&w) = edges[v].get(*e) {
                *e += 1;
                if order[w] == NONE {
                    order[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(order[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(u, _)) = work.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == order[v] {
                let mut component = vec![];
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                let cycle = component.len() > 1 || edges[v].contains(&v);
                if cycle && component.iter().all(|&w| object(aliases[w])) {
                    cyclic.extend(
                        component
                            .iter()
                            .map(|&w| (aliases[w].name.name.as_str(), v)),
                    );
                }
            }
        }
    }
    cyclic
}

/// An object type's field as an interface declares it (the type without the `| null` that the
/// parser adds to an optional field of an object type).
fn interface_field(f: &ast::ObjectTypeField) -> ast::Field {
    let ty = match f.optional {
        true => crate::resolve::written_type(&f.ty),
        false => f.ty.clone(),
    };
    ast::Field {
        name: f.name.clone(),
        ty,
        default: None,
        readonly: f.readonly,
        optional: f.optional,
        is_private: false,
        is_static: false,
        span: f.span,
    }
}

/// Does type `t` name `name` (a one-segment named type) anywhere?
pub(crate) fn names(t: &ast::TypeExpr, name: &str) -> bool {
    any_name(t, &mut |n| n == name)
}

/// Does `f` hold for a one-segment named type `t` names anywhere?
fn any_name(t: &ast::TypeExpr, f: &mut dyn FnMut(&str) -> bool) -> bool {
    use ast::TypeExprKind as K;
    match &t.kind {
        K::Named { path, args } => {
            (path.len() == 1 && f(&path[0].name)) || args.iter().any(|a| any_name(a, f))
        }
        K::Array(e) => any_name(e, f),
        K::Tuple(ts) | K::Union(ts) | K::Intersection(ts) => ts.iter().any(|x| any_name(x, f)),
        K::Function {
            params,
            ret,
            throws,
            ..
        } => {
            params.iter().any(|x| any_name(x, f))
                || any_name(ret, f)
                || throws.as_deref().is_some_and(|x| any_name(x, f))
        }
        K::GenericFunction { ty, .. }
        | K::Construct(ty)
        | K::TupleRest(ty)
        | K::TupleOptional(ty) => any_name(ty, f),
        K::Indexed { object, key } => any_name(object, f) || any_name(key, f),
        K::Object(fs) => fs.iter().any(|x| any_name(&x.ty, f)),
        K::IndexSig {
            fields, key, value, ..
        } => {
            fields.iter().any(|x| any_name(&x.ty, f)) || any_name(&key.ty, f) || any_name(value, f)
        }
        K::Callable { sigs, members } => {
            any_name(&crate::call_sigs::lower(sigs, members, t.span), f)
        }
        K::KeyOf(t) => any_name(t, f),
        K::Conditional(c) => [&c.check, &c.extends, &c.then, &c.else_]
            .iter()
            .any(|x| any_name(x, f)),
        K::Mapped(m) => {
            any_name(&m.source, f)
                || m.name.as_ref().is_some_and(|x| any_name(x, f))
                || any_name(&m.value, f)
        }
        K::TemplateLiteral { types, .. } => types.iter().any(|x| any_name(x, f)),
        K::Infer(_) => false,
        K::Predicate { ty, .. } => ty.as_deref().is_some_and(|x| any_name(x, f)),
        K::Literal(_) | K::Null | K::Undefined | K::Void | K::Query(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use velt_common::FileId;

    use super::Expander;

    /// The parts visited when rewriting the aliases of `src`, and how many became interfaces.
    fn work(src: &str) -> (u64, usize) {
        let (m, d) = velt_syntax::parse_file(FileId(0), src);
        assert!(d.is_empty(), "{d:?}");
        let mut x = Expander::new(&m.items);
        let rewritten = m.items.iter().filter_map(|i| x.as_interface(i)).count();
        assert!(x.blocked.is_empty(), "{:?}", x.blocked);
        (x.steps, rewritten)
    }

    /// `type T = A0 & { kids: T[] }` with `A{k} = A{k+1} & A{k+1}`: 2^n paths to `A{n}`'s field.
    fn shared(n: usize) -> String {
        let mut s = String::from("type T = A0 & { kids: T[] };\n");
        for k in 0..n {
            s += &format!("type A{k} = A{n1} & A{n1};\n", n1 = k + 1);
        }
        s + &format!("type A{n} = {{ x: number }};\n")
    }

    /// `type T = B0 & { kids: T[] }` with `B{k} = B{k+1} & { f{k}: number }`: one recursive
    /// alias with `n` fields, and `n` aliases that are not recursive.
    fn chain(n: usize) -> String {
        let mut s = String::from("type T = B0 & { kids: T[] };\n");
        for k in 0..n {
            s += &format!("type B{k} = B{n1} & {{ f{k}: number }};\n", n1 = k + 1);
        }
        s + &format!("type B{n} = {{ last: number }};\n")
    }

    /// The work per alias may not grow by more than `slack`× from `sizes[0]` to `sizes[1]`: the
    /// work is counted, so the comparison is exact.
    fn assert_linear(program: fn(usize) -> String, rewritten: usize) {
        let sizes = [100, 1600];
        let per_alias: Vec<f64> = sizes
            .iter()
            .map(|&n| {
                let (steps, r) = work(&program(n));
                assert_eq!(r, rewritten, "aliases rewritten for N = {n}");
                steps as f64 / n as f64
            })
            .collect();
        assert!(
            per_alias[1] < 1.5 * per_alias[0],
            "work per alias grew from {} (N = {}) to {} (N = {})",
            per_alias[0],
            sizes[0],
            per_alias[1],
            sizes[1]
        );
    }

    #[test]
    fn aliases_named_many_times_are_expanded_once() {
        // `A{n}` is one part of `T`, however many times it is named.
        assert_linear(shared, 1);
    }

    #[test]
    fn only_recursive_aliases_are_expanded() {
        assert_linear(chain, 1);
    }
}
