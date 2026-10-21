//! Recursive type aliases of object types (#654): `type Tree = { kids: Tree[] } & { v: number }`
//! and `type Tree = { kids: Tree[]; v: number }`. An alias is expanded where it is used, so one
//! that names itself would expand without end; an interface is a definition that its own fields
//! can name. Such an alias, whose body is an object type or an intersection of object types
//! written out (`{ … } & { … }`) with no field named twice, is checked as the field-only
//! interface with the same fields in the same order (`collect::field_only`): an object type
//! like the alias's, with the same layout and printing, the same name in messages, and the same
//! generics. Any other alias naming itself stays an error (`resolve::expand_alias`). The syntax
//! is rewritten before collection, like `generic_arrows`. A part may also name a non-generic
//! alias of such a type in the same module (`type Tree = Base & { kids: Tree[] }`, #783), whose
//! fields stand where its name does.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_syntax::ast;

use crate::SourceModule;

/// The modules with their recursive object type aliases rewritten into interfaces, or `None`
/// when no module has one (the common case: nothing is copied).
pub(crate) fn rewrite(modules: &[SourceModule]) -> Option<Vec<SourceModule>> {
    let rewritten: Vec<Vec<Option<ast::Item>>> = modules
        .iter()
        .map(|m| {
            let mut x = Expander::new(&m.ast.items);
            m.ast.items.iter().map(|i| x.as_interface(i)).collect()
        })
        .collect();
    if rewritten.iter().flatten().all(Option::is_none) {
        return None;
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
    Some(modules)
}

/// Expands one module's recursive object type aliases into their fields. A part may name an
/// alias (`Base` in `type Tree = Base & { kids: Tree[] }`, #783). Only aliases on a cycle of
/// aliases are expanded, and one expansion visits each alias at most once: a second visit is
/// a field named twice (or an alias without fields), so aliases naming each other many times
/// (`type A0 = A1 & A1`) cost their number, not the number of paths through them.
struct Expander<'a> {
    /// The module's non-generic type aliases, by name.
    aliases: FxHashMap<&'a str, &'a ast::TypeAlias>,
    /// The aliases on a cycle of aliases naming each other (or themselves): only those can be
    /// recursive, so no other alias is expanded.
    cyclic: FxHashSet<&'a str>,
    /// The current expansion: the aliases being expanded, the aliases visited (and whether
    /// they gave fields), and the names of the fields so far.
    seen: Vec<&'a str>,
    visited: FxHashMap<&'a str, bool>,
    field_names: FxHashSet<&'a str>,
    /// Parts visited, over all expansions (the unit tests' measure of work).
    steps: u64,
}

impl<'a> Expander<'a> {
    fn new(items: &'a [ast::Item]) -> Self {
        let mut aliases = FxHashMap::default();
        for i in items {
            if let ast::ItemKind::TypeAlias(a) = &i.kind {
                if a.generics.is_empty() {
                    aliases.entry(a.name.name.as_str()).or_insert(a);
                }
            }
        }
        Expander {
            aliases,
            cyclic: cyclic_aliases(items),
            seen: vec![],
            visited: FxHashMap::default(),
            field_names: FxHashSet::default(),
            steps: 0,
        }
    }

    /// `item` as an interface, if it is a recursive alias of object types written out or
    /// named.
    fn as_interface(&mut self, item: &'a ast::Item) -> Option<ast::Item> {
        let ast::ItemKind::TypeAlias(a) = &item.kind else {
            return None;
        };
        if !self.cyclic.contains(a.name.name.as_str()) {
            return None;
        }
        if !matches!(
            a.ty.kind,
            ast::TypeExprKind::Intersection(_) | ast::TypeExprKind::Object(_)
        ) {
            return None;
        }
        self.seen = vec![a.name.name.as_str()];
        self.visited.clear();
        self.field_names.clear();
        let mut parts = vec![];
        self.object_parts(&a.ty, &mut parts)?;
        let fields: Vec<ast::Field> = parts.into_iter().map(interface_field).collect();
        if fields.is_empty() || !fields.iter().any(|f| names(&f.ty, &a.name.name)) {
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

    /// Appends the fields of `t` to `out`: an object type, an intersection of them, or the name
    /// of a non-generic alias of one in the same module, in order. `None` for any other type, an
    /// alias that names one being expanded, or a field named twice.
    fn object_parts(
        &mut self,
        t: &'a ast::TypeExpr,
        out: &mut Vec<&'a ast::ObjectTypeField>,
    ) -> Option<()> {
        self.steps += 1;
        match &t.kind {
            ast::TypeExprKind::Object(fs) => {
                for f in fs {
                    if !self.field_names.insert(f.name.name.as_str()) {
                        return None;
                    }
                    out.push(f);
                }
                Some(())
            }
            ast::TypeExprKind::Intersection(ps) => {
                ps.iter().try_for_each(|p| self.object_parts(p, out))
            }
            ast::TypeExprKind::Named { path, args } if path.len() == 1 && args.is_empty() => {
                let name = path[0].name.as_str();
                if self.seen.contains(&name) {
                    return None;
                }
                // Visited before: its fields are in `out` already, so naming it again names
                // them twice, unless it has none.
                if let Some(&gave) = self.visited.get(name) {
                    return (!gave).then_some(());
                }
                let alias = *self.aliases.get(name)?;
                let before = out.len();
                self.seen.push(name);
                self.object_parts(&alias.ty, out)?;
                self.seen.pop();
                self.visited.insert(name, out.len() > before);
                Some(())
            }
            _ => None,
        }
    }
}

/// The type aliases among `items` that lie on a cycle of aliases naming each other (or
/// themselves): Tarjan's strongly connected components, without recursion, in time linear in
/// the aliases and the names they mention.
fn cyclic_aliases(items: &[ast::Item]) -> FxHashSet<&str> {
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
                out.extend(index.get(n));
                false
            });
            out
        })
        .collect();
    const NONE: usize = usize::MAX;
    let n = aliases.len();
    let (mut order, mut low, mut on_stack) = (vec![NONE; n], vec![0; n], vec![false; n]);
    let (mut stack, mut next, mut cyclic) = (vec![], 0, FxHashSet::default());
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
                if component.len() > 1 || edges[v].contains(&v) {
                    cyclic.extend(component.iter().map(|&w| aliases[w].name.name.as_str()));
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
        // `T`'s parts name `x` twice, so it stays an alias (and an error).
        assert_linear(shared, 0);
    }

    #[test]
    fn only_recursive_aliases_are_expanded() {
        assert_linear(chain, 1);
    }
}
