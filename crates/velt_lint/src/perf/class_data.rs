//! `perf/class-as-data`: a class that only holds data (fields, a constructor that stores its
//! parameters, no methods, no base class). Every instance is its own heap allocation behind a
//! pointer and a count; an object type stores the fields inline, so an array of them is one
//! block (measured: under a third of the instructions to build and sum 10 000 points).
//!
//! Values of an object type are copied where class instances are shared, so the rule reports a
//! class only when nothing could tell: no field of it is assigned outside its constructor, no
//! two are compared, nothing uses `instanceof` on it or names it as a value, and it is no `Map`
//! or `Set` key. The fix (also TypeScript) declares `type C = { … }` and writes each `new C(…)`
//! as `{ … } as C`, in every loaded file.

use std::collections::{HashMap, HashSet};

use velt_common::{Applicability, Fix, Span};
use velt_sema::ide::{DefKind, TypeView};
use velt_syntax::ast::{self, BinaryOp, ExprKind, StmtKind, TypeExprKind};
use velt_syntax::visit::{walk_module, Visit};

use super::Cx;
use crate::{Finding, ProfiledFix};

pub(crate) const CODE: &str = "perf/class-as-data";

/// At most this many fields: a copy of a larger value costs more than sharing it.
const MAX_FIELDS: usize = 8;

pub(super) fn find(cx: &Cx) -> Vec<Finding> {
    let mut uses: Option<Uses> = None;
    let mut out = vec![];
    for item in &cx.module.ast.items {
        let ast::ItemKind::Class(c) = &item.kind else {
            continue;
        };
        let Some(shape) = Shape::of(cx, c) else {
            continue;
        };
        let uses = uses.get_or_insert_with(|| Uses::collect(cx));
        out.extend(check(cx, item, c, &shape, uses));
    }
    out
}

/// A data class: its fields (name, `readonly`, type text) and how `new` fills each.
struct Shape {
    fields: Vec<(String, bool, String)>,
    /// Per field: the constructor parameter it is set from, or its default's text.
    from: Vec<Source>,
}

enum Source {
    Param(usize),
    Default(String),
}

impl Shape {
    fn of(cx: &Cx, c: &ast::TypeDecl) -> Option<Shape> {
        let plain = c.generics.is_empty()
            && c.extends.is_none()
            && c.implements.is_empty()
            && c.methods.is_empty()
            && !c.is_abstract
            && c.ctor_visibility == ast::CtorVisibility::Public
            && (1..=MAX_FIELDS).contains(&c.fields.len());
        if !plain
            || c.fields
                .iter()
                .any(|f| f.is_static || f.is_private || f.optional)
        {
            return None;
        }
        let params: &[ast::Param] = c.constructor.as_ref().map_or(&[], |k| &k.sig.params);
        if params
            .iter()
            .any(|p| p.pattern.is_some() || p.default.is_some() || p.rest || p.optional)
        {
            return None;
        }
        // Each statement of the constructor stores one parameter in one field.
        let mut set: HashMap<String, usize> = HashMap::new();
        for s in c.constructor.iter().flat_map(|k| &k.body.stmts) {
            let (field, value) = store(s)?;
            let i = params.iter().position(|p| p.name.name == value)?;
            if set.insert(field, i).is_some() || set.values().filter(|&&j| j == i).count() > 1 {
                return None;
            }
        }
        if set.len() != params.len() {
            return None;
        }
        let mut from = vec![];
        for f in &c.fields {
            from.push(match (set.remove(&f.name.name), &f.default) {
                (Some(i), _) => Source::Param(i),
                (None, Some(d)) => Source::Default(cx.text(d.span).to_string()),
                (None, None) => return None,
            });
        }
        if !set.is_empty() {
            return None;
        }
        let fields = c
            .fields
            .iter()
            .map(|f| {
                (
                    f.name.name.clone(),
                    f.readonly,
                    cx.text(f.ty.span).to_string(),
                )
            })
            .collect();
        Some(Shape { fields, from })
    }

    /// `{ x: a, y: b }` for a `new` with the argument texts `args`, in the parameters' order (so
    /// the arguments run in the same order), then the defaults.
    fn literal(&self, args: &[&str]) -> Option<String> {
        let mut parts: Vec<(usize, String)> = vec![];
        for ((name, _, _), from) in self.fields.iter().zip(&self.from) {
            match from {
                Source::Param(i) => parts.push((*i, format!("{name}: {}", args.get(*i)?))),
                Source::Default(d) => parts.push((usize::MAX, format!("{name}: {d}"))),
            }
        }
        parts.sort_by_key(|(i, _)| *i);
        let parts: Vec<String> = parts.into_iter().map(|(_, p)| p).collect();
        Some(format!("{{ {} }}", parts.join(", ")))
    }

    fn alias(&self, name: &str, exported: bool) -> String {
        let fields: Vec<String> = self
            .fields
            .iter()
            .map(|(n, readonly, ty)| {
                format!("{}{n}: {ty}", if *readonly { "readonly " } else { "" })
            })
            .collect();
        let export = if exported { "export " } else { "" };
        format!("{export}type {name} = {{ {} }};", fields.join("; "))
    }
}

/// `this.f = p;`: the field and the parameter's name.
fn store(s: &ast::Stmt) -> Option<(String, String)> {
    let StmtKind::Expr(e) = &s.kind else {
        return None;
    };
    let ExprKind::Assign {
        op: None,
        target,
        value,
    } = &e.kind
    else {
        return None;
    };
    let (ExprKind::Member { object, prop, .. }, ExprKind::Ident(v)) = (&target.kind, &value.kind)
    else {
        return None;
    };
    matches!(object.kind, ExprKind::This).then(|| (prop.name.clone(), v.name.clone()))
}

/// What the loaded user modules do with classes, collected once per module linted.
#[derive(Default)]
struct Uses {
    /// The class name's span in each `new C(…)`, with the expression and its argument spans.
    news: HashMap<Span, (Span, Vec<Span>)>,
    /// `new` expressions that need parentheses around `{ … } as C` (an operand, a callee, an
    /// object of `.`).
    tight: HashSet<Span>,
    /// Spans of names in written types.
    types: HashSet<Span>,
    /// Spans of names inside `Map`/`Set`/`WeakMap`/`WeakSet` types.
    keyed: Vec<Span>,
    /// Spans of names in `import { … }` and `export { … }` lists.
    imports: HashSet<Span>,
    /// Spans of `instanceof` targets.
    instanceof: HashSet<Span>,
    /// Objects whose field is assigned, and operands of `==`/`!=`/`===`/`!==`.
    assigned: Vec<Span>,
    compared: Vec<Span>,
}

impl Uses {
    fn collect(cx: &Cx) -> Uses {
        let mut uses = Uses::default();
        for m in cx.modules.iter().filter(|m| !m.is_std) {
            walk_module(&m.ast, &mut uses);
        }
        uses
    }
}

impl<'a> Visit<'a> for Uses {
    fn item(&mut self, item: &'a ast::Item) {
        if let ast::ItemKind::Import(i) = &item.kind {
            for n in &i.names {
                self.imports.insert(n.name.span);
                self.imports.extend(n.alias.as_ref().map(|a| a.span));
            }
        }
    }

    fn ty(&mut self, t: &'a ast::TypeExpr) {
        if let TypeExprKind::Named { path, .. } = &t.kind {
            self.types.extend(path.iter().map(|p| p.span));
            let keyed = matches!(
                path.last().map(|p| p.name.as_str()),
                Some("Map" | "Set" | "WeakMap" | "WeakSet")
            );
            if keyed {
                self.keyed.push(t.span);
            }
        }
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        match &e.kind {
            ExprKind::New { class, args } => {
                if let TypeExprKind::Named { path, args: targs } = &class.kind {
                    if let ([name], true) = (path.as_slice(), targs.is_empty()) {
                        let args = args.iter().map(|a| a.span).collect();
                        self.news.insert(name.span, (e.span, args));
                    }
                }
            }
            ExprKind::InstanceOf { ty, .. } => {
                if let TypeExprKind::Named { path, .. } = &ty.kind {
                    self.instanceof.extend(path.iter().map(|p| p.span));
                }
            }
            ExprKind::Assign { target, .. } | ExprKind::Update { target, .. } => {
                if let ExprKind::Member { object, .. } | ExprKind::Index { object, .. } =
                    &target.kind
                {
                    self.assigned.push(object.span);
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                if matches!(
                    op,
                    BinaryOp::Eq | BinaryOp::NotEq | BinaryOp::StrictEq | BinaryOp::StrictNotEq
                ) {
                    self.compared.extend([lhs.span, rhs.span]);
                }
                self.tight.extend([lhs.span, rhs.span]);
            }
            ExprKind::Member { object, .. } | ExprKind::Index { object, .. } => {
                self.tight.insert(object.span);
            }
            ExprKind::Call { callee, .. } => {
                self.tight.insert(callee.span);
            }
            ExprKind::Unary { expr, .. }
            | ExprKind::Cast { expr, .. }
            | ExprKind::NonNull(expr) => {
                self.tight.insert(expr.span);
            }
            _ => {}
        }
    }
}

fn check(
    cx: &Cx,
    item: &ast::Item,
    c: &ast::TypeDecl,
    shape: &Shape,
    uses: &Uses,
) -> Option<Finding> {
    let def = cx.analysis.def_of(c.name.span)?;
    if def.kind != DefKind::Class {
        return None;
    }
    let is_class = |span: Span| {
        cx.analysis
            .type_of(span)
            .is_some_and(|t| match cx.analysis.view(&t) {
                TypeView::Named(n) => n.name == c.name.name && !n.is_std,
                TypeView::Nullable(inner) => {
                    matches!(cx.analysis.view(&inner), TypeView::Named(n) if n.name == c.name.name)
                }
                _ => false,
            })
    };
    let inside = |s: Span| s.file == item.span.file && item.span.lo <= s.lo && s.hi <= item.span.hi;
    // Copies would show: a field assigned outside the constructor, or two compared.
    if uses.assigned.iter().any(|s| !inside(*s) && is_class(*s))
        || uses.compared.iter().any(|s| is_class(*s))
    {
        return None;
    }
    let mut news = vec![];
    for r in cx.analysis.references(&def) {
        if r == c.name.span {
            continue;
        }
        if let Some(new) = uses.news.get(&r) {
            news.push(new);
            continue;
        }
        let keyed = uses
            .keyed
            .iter()
            .any(|k| k.file == r.file && k.lo <= r.lo && r.hi <= k.hi);
        if uses.imports.contains(&r) {
            continue;
        }
        if !uses.types.contains(&r) || uses.instanceof.contains(&r) || keyed {
            return None;
        }
    }
    let name = &c.name.name;
    let mut notes = vec![
        format!(
            "an object type (`type {name} = {{ … }}`) stores its fields inline, so an array of \
             them is one block"
        ),
        "values of an object type are copied, not shared: nothing here assigns a field \
         outside the constructor, compares two or uses `instanceof`, so nothing can tell"
            .into(),
    ];
    let message = format!("`{name}` only holds data: each instance is a separate heap allocation");
    if item.exported && !cx.whole_package {
        // Files that import it are not loaded here: their uses would stay classes.
        notes.push(
            "`velt check --perf --fix=review` in the package rewrites it with every file that uses it"
                .to_string(),
        );
        return Some(Finding {
            code: CODE,
            span: c.name.span,
            message,
            notes,
            fixes: vec![],
        });
    }
    let mut edits = vec![(item.span, shape.alias(name, item.exported))];
    for (span, args) in news {
        let args: Vec<&str> = args.iter().map(|a| cx.text(*a)).collect();
        let literal = format!("{} as {name}", shape.literal(&args)?);
        let text = if uses.tight.contains(span) {
            format!("({literal})")
        } else {
            literal
        };
        edits.push((*span, text));
    }
    edits.sort_by_key(|(s, _)| (s.file, s.lo));
    Some(Finding {
        code: CODE,
        span: c.name.span,
        message,
        notes,
        fixes: vec![ProfiledFix {
            fix: Fix {
                title: format!("make `{name}` an object type"),
                edits,
                applicability: Applicability::Review,
            },
            portable: true,
        }],
    })
}
