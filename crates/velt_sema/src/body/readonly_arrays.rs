//! Read-only arrays (docs/reference/types.md "Read-only arrays"): `readonly T[]` and
//! `ReadonlyArray<T>` are `T[]` (the parser reads the first as the second), and a variable or
//! parameter declared with one of them may not be written through: a mutating method
//! (`push`, `sort`, …) is TypeScript's TS2339 and an element assignment TS2542. The check
//! follows the variable, not the value: a copy into a `T[]` variable is not checked (TypeScript
//! rejects that assignment, TS4104). `ReadonlySet<T>` and `ReadonlyMap<K, V>` are `Set<T>` and
//! `Map<K, V>` (prelude aliases), checked the same way: through a variable declared with one,
//! the methods that change the collection (`add`, `set`, `delete`, `clear`, ...) are TS2339.
//!
//! The variable may also be declared through a type alias (`type Tags = ReadonlySet<string>`)
//! or initialized with a cast to one (`new Set() as ReadonlySet<string>`), and the check follows
//! it into closures that capture it, through narrowing (`if (s)`, `s!`), optional chains
//! (`s?.add(x)`) and into a `const` (or `let`) it is copied to without a type (`const t = s`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::FnCx;
use crate::hir::{self, ExprKind as H, LitValue, LocalId, TyId, TyKind};

/// [`super::Frame::local_marks`]: a local declared with a read-only type.
pub(crate) const READONLY: u8 = 1;
/// [`super::Frame::local_marks`]: a local declared with a number-keyed record type.
pub(crate) const NUMBER_RECORD: u8 = 2;

impl FnCx<'_, '_> {
    /// Marks local `l` with `mark` ([`super::Frame::local_marks`]).
    pub(crate) fn mark_local(&mut self, l: LocalId, mark: u8) {
        *self.f.local_marks.entry(l).or_default() |= mark;
    }

    /// Does local `l` have `mark`?
    pub(crate) fn local_marked(&self, l: LocalId, mark: u8) -> bool {
        self.f.local_marks.get(&l).is_some_and(|m| m & mark != 0)
    }

    /// Records local `l` declared with the written type `ty` when that is a read-only array.
    pub(super) fn note_readonly_array(&mut self, l: LocalId, ty: Option<&ast::TypeExpr>) {
        if ty.is_some_and(|t| self.may_be_marked(t) && self.is_readonly_written(t, 0)) {
            self.mark_local(l, READONLY);
        }
    }

    /// [`Self::note_readonly_array`] and `note_number_record` for local `l` declared with the
    /// written type `ty` (and initialized with `init`), behind one cheap filter.
    pub(super) fn note_marks(
        &mut self,
        l: LocalId,
        ty: Option<&ast::TypeExpr>,
        init: Option<&ast::Expr>,
    ) {
        let Some(t) = ty.filter(|t| self.may_be_marked(t)) else {
            return;
        };
        if self.is_readonly_written(t, 0) {
            self.mark_local(l, READONLY);
        }
        self.note_number_record(l, t, init);
    }

    /// Can the written type `t` be a read-only or number-keyed record type? A cheap filter
    /// before the checks: most declared types are neither.
    pub(crate) fn may_be_marked(&self, t: &ast::TypeExpr) -> bool {
        match &t.kind {
            ast::TypeExprKind::Named { path, .. } => {
                let name = &path[path.len() - 1].name;
                name.starts_with('R') || {
                    let (ro, num) = self.cx.marked_aliases();
                    !(ro.is_empty() && num.is_empty())
                }
            }
            ast::TypeExprKind::Union(_) | ast::TypeExprKind::IndexSig { .. } => true,
            _ => false,
        }
    }

    /// Records local `l`, declared without a type, as read-only when its initializer `init`
    /// (`ast`, checked as `h`) is a cast to a read-only type or a read-only variable.
    pub(super) fn note_readonly_init(&mut self, l: LocalId, init: &ast::Expr, h: &hir::Expr) {
        // A cast decides: to a read-only type it marks, to another (`xs as string[]`, how
        // TypeScript code gets past `readonly`) it does not.
        let marked = match &strip_parens(init).kind {
            ast::ExprKind::Cast { ty, .. } => self.is_readonly_written(ty, 0),
            _ => self.readonly_local(h).is_some(),
        };
        if marked {
            self.mark_local(l, READONLY);
        }
    }

    /// The receiver `recv` of a method call written `(x as T).m()`: with `T` not read-only the
    /// call may change it, with `T` read-only it may not ([`Frame::readonly_exempt`](super::Frame)
    /// and `readonly_cast_recv`, taken by the check).
    pub(crate) fn exempt_cast_receiver(&mut self, object: &ast::Expr, recv: &hir::Expr) {
        if let ast::ExprKind::Cast { ty, .. } = &strip_parens(object).kind {
            match self.is_readonly_written(ty, 0) {
                // `(xs as readonly string[]).push(x)`: TS2339 as through a variable.
                true => self.f.readonly_cast_recv = true,
                false => self.f.readonly_exempt = self.readonly_local(recv),
            }
        }
    }

    /// The read-only locals `cond` tests with `x instanceof Set` (`Map`, `Array`), alone or in an
    /// `&&` chain: TypeScript narrows them to the mutable class there, so their mark is lifted
    /// for the branch where `cond` holds. What to give [`Self::restore_marks`] after it.
    pub(super) fn instanceof_unmark(&mut self, cond: &ast::Expr) -> Vec<(LocalId, u8)> {
        let mut out = vec![];
        if self.f.local_marks.is_empty() {
            return out;
        }
        self.instanceof_unmark_into(cond, &mut out);
        out
    }

    fn instanceof_unmark_into(&mut self, cond: &ast::Expr, out: &mut Vec<(LocalId, u8)>) {
        match &cond.kind {
            ast::ExprKind::Paren(x) => self.instanceof_unmark_into(x, out),
            ast::ExprKind::Binary {
                op: ast::BinaryOp::And,
                lhs,
                rhs,
            } => {
                self.instanceof_unmark_into(lhs, out);
                self.instanceof_unmark_into(rhs, out);
            }
            ast::ExprKind::InstanceOf { expr, ty } => {
                let ast::ExprKind::Ident(x) = &strip_parens(expr).kind else {
                    return;
                };
                let mutable_class = matches!(&ty.kind, ast::TypeExprKind::Named { path, .. }
                    if path.len() == 1 && matches!(path[0].name.as_str(), "Set" | "Map" | "Array"));
                if self.f.local_marks.is_empty() || !mutable_class {
                    return;
                }
                let Some(l) = self.lookup_local(&x.name, x.span) else {
                    return;
                };
                if let Some(m) = self
                    .f
                    .local_marks
                    .get_mut(&l)
                    .filter(|m| **m & READONLY != 0)
                {
                    out.push((l, *m));
                    *m &= !READONLY;
                }
            }
            _ => {}
        }
    }

    /// Put back the marks [`Self::instanceof_unmark`] lifted.
    pub(super) fn restore_marks(&mut self, marks: Vec<(LocalId, u8)>) {
        for (l, m) in marks {
            self.f.local_marks.insert(l, m);
        }
    }

    /// Records the binding `l` of an optional chain on `object` as read-only when `object` is
    /// a read-only variable.
    pub(crate) fn note_readonly_binding(&mut self, l: LocalId, object: &hir::Expr) {
        if self.readonly_local(object).is_some() {
            self.mark_local(l, READONLY);
        }
    }

    /// The read-only variable `h` reads (through casts, narrowing and upcasts).
    fn readonly_local(&self, h: &hir::Expr) -> Option<LocalId> {
        if self.f.local_marks.is_empty() {
            return None;
        }
        match &h.kind {
            H::Local(l, _) => self.local_marked(*l, READONLY).then_some(*l),
            H::Cast(x) | H::UnwrapSome(x, _) | H::Upcast(x) | H::Downcast(x) => {
                self.readonly_local(x)
            }
            // `s!`: a match on `s` that gives its payload.
            H::Match { scrutinee, .. } => self.readonly_local(scrutinee),
            _ => None,
        }
    }

    /// Is `t` a read-only array or collection type, also through type aliases of the module?
    fn is_readonly_written(&self, t: &ast::TypeExpr, depth: usize) -> bool {
        if is_readonly_array(t) || is_readonly_collection(t) {
            return true;
        }
        self.through_alias(t, depth, 0, &|cx, t, d| cx.is_readonly_written(t, d))
    }

    /// `test` of what `t` names when it is one of the program's aliases in `marked_aliases()`
    /// set `set` (generic or not: the body decides, whatever the arguments), or of the one
    /// member of `t | null` / `t | undefined`.
    pub(crate) fn through_alias(
        &self,
        t: &ast::TypeExpr,
        depth: usize,
        set: usize,
        test: &dyn Fn(&Self, &ast::TypeExpr, usize) -> bool,
    ) -> bool {
        if depth > 8 {
            return false;
        }
        let path = match &t.kind {
            ast::TypeExprKind::Named { path, .. } => path,
            ast::TypeExprKind::Union(ms) => {
                let mut rest = ms.iter().filter(|m| {
                    !matches!(
                        m.kind,
                        ast::TypeExprKind::Null | ast::TypeExprKind::Undefined
                    )
                });
                return match (rest.next(), rest.next()) {
                    (Some(m), None) => test(self, m, depth + 1),
                    _ => false,
                };
            }
            _ => return false,
        };
        let marked = match set {
            0 => &self.cx.marked_aliases().0,
            _ => &self.cx.marked_aliases().1,
        };
        if path.len() != 1 || marked.is_empty() || !marked.contains(&path[0].name) {
            return false;
        }
        match self
            .cx
            .lookup_item_at(self.module, &path[0].name, path[0].span)
        {
            Some(crate::ctx::Item::Alias(a)) => {
                test(self, &self.cx.aliases[a as usize].decl.ty, depth + 1)
            }
            _ => false,
        }
    }

    /// The readonly array type local `l` (of type `ty`) is declared with, as TypeScript shows
    /// it (`readonly number[]`).
    fn readonly_shown(&mut self, h: &hir::Expr) -> Option<String> {
        self.readonly_local(h)?;
        self.readonly_type_shown(h.ty)
    }

    /// The read-only type over `t` (`T[]`, `Set<T>`, `Map<K, V>`) as TypeScript shows it.
    fn readonly_type_shown(&mut self, t: TyId) -> Option<String> {
        let TyKind::Array(elem) = *self.cx.ty.kind(t) else {
            // `ReadonlySet<T>` / `ReadonlyMap<K, V>`, shown as TypeScript does.
            let shown = self.cx.display(t);
            let col = shown.starts_with("Set<") || shown.starts_with("Map<");
            return col.then(|| format!("Readonly{shown}"));
        };
        let shown = self.cx.display(elem);
        match self.cx.union_members(elem) {
            Some(_) if shown.contains(" | ") => Some(format!("readonly ({shown})[]")),
            _ => Some(format!("readonly {shown}[]")),
        }
    }

    /// Place `e` is about to be assigned: an error when it is an element of a read-only array
    /// variable.
    pub(super) fn readonly_element_write(&mut self, e: &hir::Expr) {
        let H::Index { base, .. } = &e.kind else {
            return;
        };
        if let Some(shown) = self.readonly_shown(base) {
            self.cx.error(Diagnostic::error(
                format!(
                    "index signature in type '{shown}' only permits reading (TypeScript's TS2542)"
                ),
                e.span,
            ));
        }
    }

    /// A call of method `name` (with `args`) on `recv`: an error when it is a mutating method of
    /// an array and `recv` a read-only array variable.
    pub(crate) fn readonly_method_call(
        &mut self,
        recv: &hir::Expr,
        name: &str,
        args: &[ast::Expr],
        span: Span,
    ) {
        let exempt = self.f.readonly_exempt.take();
        let forced = std::mem::take(&mut self.f.readonly_cast_recv);
        if exempt.is_some() && exempt == self.readonly_local(recv) {
            return;
        }
        let array = matches!(self.cx.ty.kind(recv.ty), TyKind::Array(_));
        if array && !MUTATING.contains(&name) {
            self.array_method_in_place(recv, name, args);
            return;
        }
        if !array && !COLLECTION_MUTATING.contains(&name) {
            return;
        }
        let shown = match forced {
            true => self.readonly_type_shown(recv.ty),
            false => self.readonly_shown(recv),
        };
        if let Some(shown) = shown {
            if !array {
                self.cx.error(
                    Diagnostic::error(
                        format!("property '{name}' does not exist on type '{shown}' (TypeScript's TS2339)"),
                        span,
                    )
                    .with_note("a read-only collection has no methods that change it; copy it first (`new Set(s)`, `new Map(m)`) to get one you may change"),
                );
                return;
            }
            self.cx.error(
                Diagnostic::error(
                    format!("property '{name}' does not exist on type '{shown}' (TypeScript's TS2339)"),
                    span,
                )
                .with_note("a read-only array has no methods that change it; copy it first (`[...xs]`) to get an array you may change"),
            );
        }
    }
}

impl crate::ctx::Ctx<'_> {
    /// `ReadonlyMap<K, V>` (unless an item of that name is in scope): `Map<K, V>`. It is a built-in
    /// name rather than a prelude alias, which every program would pay for.
    pub(crate) fn resolve_readonly_map(
        &mut self,
        t: &ast::TypeExpr,
        args: &[ast::TypeExpr],
        env: &crate::resolve::TyEnv,
    ) -> TyId {
        let args: Vec<TyId> = args.iter().map(|a| self.resolve_type(a, env)).collect();
        let Some(map) = self.prelude_adt("Map").filter(|_| args.len() == 2) else {
            self.arity_error("ReadonlyMap", 2, args.len(), t);
            return self.ty.error;
        };
        self.ty.intern(TyKind::Adt(map, args))
    }
}

impl FnCx<'_, '_> {
    /// `e as const`. An array literal of string literals with no expected type keeps their
    /// literal types (TypeScript's `readonly ["a", "b"]`): it is an array of their union (of
    /// the one literal type, `"a"[]`, for `["a"] as const`), so iterating or indexing it gives
    /// `"a" | "b"`. Anything else is typed as `e` alone: Velt arrays and literals need no
    /// annotation for it.
    pub(super) fn as_const(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        want: super::Want,
    ) -> hir::Expr {
        let exp = match exp {
            Some(t) => Some(t),
            None => self.as_const_array_ty(e),
        };
        self.expr(e, exp, want)
    }

    fn as_const_array_ty(&mut self, e: &ast::Expr) -> Option<TyId> {
        let elems = as_const_strings(e)?;
        let mut members = vec![];
        for s in elems {
            let t = self.cx.lit_type(LitValue::Str(s.clone()));
            if !members.contains(&t) {
                members.push(t);
            }
        }
        let u = match members[..] {
            [one] => one,
            _ => self.cx.union_of(&members, false, e.span),
        };
        Some(self.cx.ty.array(u))
    }

    /// Records local `l` initialized with `init` as a read-only array when that is an array
    /// literal of strings `as const`.
    pub(super) fn note_as_const_array(&mut self, l: LocalId, init: Option<&ast::Expr>) {
        let Some(ast::ExprKind::Cast { expr, ty }) = init.map(|e| &e.kind) else {
            return;
        };
        if super::expr::member::is_as_const(ty) {
            if let Some(names) = as_const_tuple(expr) {
                self.f.as_const_tuples.insert(l, names);
            }
        }
        if super::expr::member::is_as_const(ty) && as_const_strings(expr).is_some() {
            self.mark_local(l, READONLY);
        }
    }
}

/// `[2, "y"]` (in parentheses or not): its elements when they are literals of more than one
/// type (TypeScript's `readonly [2, "y"]` as `as const` gives it).
pub(super) fn as_const_tuple(e: &ast::Expr) -> Option<Vec<ast::Expr>> {
    let elems = match &e.kind {
        ast::ExprKind::Paren(inner) => return as_const_tuple(inner),
        ast::ExprKind::Array(elems) if elems.len() > 1 => elems,
        _ => return None,
    };
    let names: Vec<&'static str> = elems.iter().map(literal_type).collect::<Option<_>>()?;
    (names.iter().any(|n| *n != names[0])).then(|| elems.clone())
}

/// The type of literal `x` as an element of a tuple `as const` gives it (`"number"` for `1`),
/// if `x` is a string, number or boolean literal.
pub(super) fn literal_type(x: &ast::Expr) -> Option<&'static str> {
    match &x.kind {
        ast::ExprKind::Lit(ast::Lit::Str(_)) => Some("string"),
        ast::ExprKind::Lit(
            ast::Lit::Int { suffix: None, .. } | ast::Lit::Float { suffix: None, .. },
        ) => Some("number"),
        ast::ExprKind::Lit(ast::Lit::Bool(_)) => Some("boolean"),
        _ => None,
    }
}

/// The elements of `[...]` (in parentheses or not) when it has some and they are all string
/// literals.
fn as_const_strings(e: &ast::Expr) -> Option<Vec<&String>> {
    match &e.kind {
        ast::ExprKind::Paren(inner) => as_const_strings(inner),
        ast::ExprKind::Array(elems) if !elems.is_empty() => elems
            .iter()
            .map(|x| match &x.kind {
                ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// The methods of `T[]` that `readonly T[]` doesn't have.
const MUTATING: [&str; 9] = [
    "push",
    "pop",
    "shift",
    "unshift",
    "splice",
    "sort",
    "reverse",
    "fill",
    "copyWithin",
];

/// The methods of `Set<T>` and `Map<K, V>` that `ReadonlySet<T>` and `ReadonlyMap<K, V>` don't
/// have.
const COLLECTION_MUTATING: [&str; 7] = [
    "add",
    "set",
    "delete",
    "clear",
    "upsert",
    "update",
    "getOrInsert",
];

/// A read-only array or collection type as written (not through aliases).
pub(crate) fn is_readonly_type(t: &ast::TypeExpr) -> bool {
    is_readonly_array(t) || is_readonly_collection(t)
}

/// `ReadonlySet<T>` or `ReadonlyMap<K, V>`, possibly `| null`.
fn is_readonly_collection(t: &ast::TypeExpr) -> bool {
    match &t.kind {
        ast::TypeExprKind::Named { path, args } => {
            path.len() == 1
                && matches!(
                    (path[0].name.as_str(), args.len()),
                    ("ReadonlySet", 1) | ("ReadonlyMap", 2)
                )
        }
        ast::TypeExprKind::Union(ms) => {
            let mut non_null = ms.iter().filter(|m| {
                !matches!(
                    m.kind,
                    ast::TypeExprKind::Null | ast::TypeExprKind::Undefined
                )
            });
            matches!((non_null.next(), non_null.next()), (Some(m), None) if is_readonly_collection(m))
        }
        _ => false,
    }
}

/// `readonly T[]` (parsed as `ReadonlyArray<T>`) or `ReadonlyArray<T>`, possibly `| null`.
pub(crate) fn is_readonly_array(t: &ast::TypeExpr) -> bool {
    match &t.kind {
        ast::TypeExprKind::Named { path, args } => {
            path.len() == 1 && path[0].name == "ReadonlyArray" && args.len() == 1
        }
        ast::TypeExprKind::Union(ms) => {
            let mut non_null = ms.iter().filter(|m| {
                !matches!(
                    m.kind,
                    ast::TypeExprKind::Null | ast::TypeExprKind::Undefined
                )
            });
            matches!((non_null.next(), non_null.next()), (Some(m), None) if is_readonly_array(m))
        }
        _ => false,
    }
}

/// `e` without the parentheses around it.
fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => strip_parens(x),
        _ => e,
    }
}
