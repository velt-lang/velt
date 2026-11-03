//! `perf/dynamic-record`: a parameter typed `Record<string, any>` (or `any`) that the function
//! reads with constant keys. Each read looks the key up in a map and converts the `any` it finds
//! (#2058: sigx's props, the largest cost of its deep-tree render); an object type with the
//! fields it reads stores them inline, so a read is a load (measured: over 200× fewer
//! instructions for two reads). When every use of the parameter is such a read converted with
//! `as T`, the fix writes that object type; it is TypeScript too.

use std::collections::HashSet;

use velt_common::{Applicability, Fix, Span};
use velt_sema::ide::DefKind;
use velt_syntax::ast::{self, ExprKind, Lit};
use velt_syntax::visit::{walk_module, Visit};

use super::Cx;
use crate::{Finding, ProfiledFix};

pub(crate) const CODE: &str = "perf/dynamic-record";

pub(super) fn find(cx: &Cx) -> Vec<Finding> {
    struct Fns<'c, 'a> {
        cx: &'c Cx<'a>,
        out: Vec<Finding>,
    }
    impl<'a> Visit<'a> for Fns<'_, '_> {
        fn function(&mut self, sig: &'a ast::FnSig, body: &'a ast::Block) {
            for p in sig.params.iter().filter(|p| p.pattern.is_none() && !p.rest) {
                if is_dynamic(self.cx, &p.ty) {
                    self.out.extend(check(self.cx, p, body));
                }
            }
        }
    }
    let mut v = Fns { cx, out: vec![] };
    walk_module(&cx.module.ast, &mut v);
    v.out
}

/// `Record<string, any>` or `any`, as written.
fn is_dynamic(cx: &Cx, ty: &ast::TypeExpr) -> bool {
    let text: String = cx.text(ty.span).split_whitespace().collect();
    text == "Record<string,any>" || text == "any"
}

/// One constant-key read of the parameter: `p.k` or `p["k"]`, converted with `as T` or not.
struct Read {
    key: String,
    /// The span of the expression to replace (the `as` expression when there is one).
    span: Span,
    /// The span of the parameter's name in it.
    name: Span,
    /// `T` of `as T`.
    ty: Option<String>,
}

fn check(cx: &Cx, p: &ast::Param, body: &ast::Block) -> Option<Finding> {
    let reads = Reads::of(cx, p, body);
    let first = reads.reads.first()?;
    let name = &p.name.name;
    let message = format!(
        "`{name}` is a `{}`: each read looks its key up and converts an `any`",
        cx.text(p.ty.span)
    );
    let mut keys: Vec<(String, String)> = vec![];
    let mut typed = true;
    for r in &reads.reads {
        match (&r.ty, keys.iter().find(|(k, _)| *k == r.key)) {
            (None, _) => typed = false,
            (Some(t), Some((_, seen))) if seen != t => typed = false,
            (Some(t), None) => keys.push((r.key.clone(), t.clone())),
            _ => {}
        }
    }
    let shape = keys
        .iter()
        .map(|(k, t)| format!("{k}: {t}"))
        .collect::<Vec<_>>()
        .join("; ");
    let read_names: HashSet<Span> = reads.reads.iter().map(|r| r.name).collect();
    let only_reads = reads.uses.iter().all(|u| read_names.contains(u)) && reads.writes == 0;
    let identifiers = keys.iter().all(|(k, _)| is_identifier(k));
    let mut notes = vec![format!(
        "an object type with the fields it reads stores them inline, so a read is a load: \
         `{name}: {{ {} }}`",
        if shape.is_empty() {
            format!("{}: …", first.key)
        } else {
            shape.clone()
        }
    )];
    let fix = (typed && only_reads && identifiers).then(|| {
        let mut edits = vec![(p.ty.span, format!("{{ {shape} }}"))];
        edits.extend(
            reads
                .reads
                .iter()
                .map(|r| (r.span, format!("{name}.{}", r.key))),
        );
        ProfiledFix {
            fix: Fix {
                title: format!("type `{name}` as `{{ {shape} }}`"),
                edits,
                // Callers must now pass objects with those fields; the checker says where one
                // doesn't.
                applicability: Applicability::Review,
            },
            portable: true,
        }
    });
    if fix.is_none() {
        notes.push(
            "`velt check --fix` rewrites it when every use is a read with a constant key, \
             converted with `as T`"
                .into(),
        );
    }
    Some(Finding {
        code: CODE,
        span: p.ty.span,
        message,
        notes,
        fixes: fix.into_iter().collect(),
    })
}

/// The uses of a parameter in a function body.
struct Reads {
    reads: Vec<Read>,
    /// Every span naming the parameter.
    uses: Vec<Span>,
    /// Assignments to a key of it, or to it.
    writes: usize,
}

impl Reads {
    fn of(cx: &Cx, p: &ast::Param, body: &ast::Block) -> Reads {
        struct V<'c, 'a> {
            cx: &'c Cx<'a>,
            param: Span,
            out: Reads,
            /// Reads already taken as the operand of an `as`.
            cast: HashSet<Span>,
        }
        impl V<'_, '_> {
            fn is_param(&self, e: &ast::Expr) -> Option<Span> {
                let ExprKind::Ident(id) = &e.kind else {
                    return None;
                };
                let d = self.cx.analysis.def_of(id.span)?;
                (d.kind == DefKind::Parameter && d.span == self.param).then_some(id.span)
            }
            /// `p.k` / `p["k"]`: the key and the parameter's span.
            fn read(&self, e: &ast::Expr) -> Option<(String, Span)> {
                match &e.kind {
                    ExprKind::Member {
                        object,
                        prop,
                        optional: false,
                    } => Some((prop.name.clone(), self.is_param(object)?)),
                    ExprKind::Index {
                        object,
                        index,
                        optional: false,
                    } => match &index.kind {
                        ExprKind::Lit(Lit::Str(k)) => Some((k.clone(), self.is_param(object)?)),
                        _ => None,
                    },
                    _ => None,
                }
            }
        }
        impl<'a> Visit<'a> for V<'_, '_> {
            fn expr(&mut self, e: &'a ast::Expr) {
                if let Some(span) = self.is_param(e) {
                    self.out.uses.push(span);
                }
                match &e.kind {
                    // `(p["k"] as T)`: the parentheses go too.
                    ExprKind::Paren(inner) => {
                        if let ExprKind::Cast { expr, ty } = &inner.kind {
                            if let Some((key, name)) = self.read(expr) {
                                self.cast.insert(inner.span);
                                self.cast.insert(expr.span);
                                let ty = Some(self.cx.text(ty.span).to_string());
                                self.out.reads.push(Read {
                                    key,
                                    span: e.span,
                                    name,
                                    ty,
                                });
                            }
                        }
                    }
                    ExprKind::Cast { .. } if self.cast.contains(&e.span) => {}
                    ExprKind::Cast { expr, ty } => {
                        if let Some((key, name)) = self.read(expr) {
                            self.cast.insert(expr.span);
                            let ty = Some(self.cx.text(ty.span).to_string());
                            self.out.reads.push(Read {
                                key,
                                span: e.span,
                                name,
                                ty,
                            });
                        }
                    }
                    ExprKind::Assign { target, .. } | ExprKind::Update { target, .. } => {
                        if self.read(target).is_some() || self.is_param(target).is_some() {
                            self.out.writes += 1;
                        }
                    }
                    _ => {
                        if let Some((key, name)) = self.read(e) {
                            if !self.cast.contains(&e.span) {
                                self.out.reads.push(Read {
                                    key,
                                    span: e.span,
                                    name,
                                    ty: None,
                                });
                            }
                        }
                    }
                }
            }
        }
        let mut v = V {
            cx,
            param: p.name.span,
            out: Reads {
                reads: vec![],
                uses: vec![],
                writes: 0,
            },
            cast: HashSet::new(),
        };
        let arrow = wrap(body);
        velt_syntax::visit::walk_expr(&arrow, &mut v);
        v.out
    }
}

/// The block as the body of an arrow, so the expression walker visits its statements.
fn wrap(body: &ast::Block) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ExprKind::Arrow {
            type_params: vec![],
            params: vec![],
            ret: None,
            throws: None,
            body: ast::ArrowBody::Block(body.clone()),
            is_async: false,
        },
        span: body.span,
    }
}

/// `[A-Za-z_$][A-Za-z0-9_$]*`: a key an object type and a `.` read can spell.
fn is_identifier(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}
