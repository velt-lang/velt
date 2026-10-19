//! The diagnostic: where the handler calls the closure, and the fix-it declaration.

use super::*;

/// Where a handler calls a function value that may be closure `c` (of type `ty`), as written
/// (`i.onChange`), with the call's span: in a request's closure first, else in another closure
/// the handler reaches. A call of a field `c` is stored in, on what `c` was stored into
/// (`i.onChange` with `i.onChange = …`), is preferred, then of that field on anything, or
/// through a variable `c` flows into, then a call through a function value of `c`'s type, then
/// of its shape.
pub(super) fn call_site(
    cx: &Ctx,
    g: &Graph,
    reached: &[(DefId, TyId, Why)],
    requests: &HashSet<DefId>,
    bodies: &Bodies,
    c: DefId,
    ty: TyId,
) -> Option<(String, Span)> {
    let mut order: Vec<DefId> = reached.iter().map(|(d, ..)| *d).collect();
    order.sort_by_key(|d| !requests.contains(d));
    let lit = g.ids.get(&Node::Lit(c)).copied();
    let homes = bodies.homes.get(&c).map(Vec::as_slice).unwrap_or_default();
    let flows = |n: usize, to: &[usize]| {
        let mut seen = HashSet::new();
        let mut work = vec![n];
        while let Some(m) = work.pop() {
            if to.contains(&m) {
                return true;
            }
            if seen.insert(m) {
                work.extend(g.srcs[m].iter().map(|(s, _)| *s));
            }
        }
        false
    };
    let ret = |t: TyId| match cx.ty.kind(t) {
        TyKind::FnPtr { ret, .. } => Some(*ret),
        _ => None,
    };
    let fields = &bodies.stores;
    let mut best: Option<(u8, bool, u32, String, Span)> = None;
    for d in order {
        if d == c || in_std(cx, d) {
            continue;
        }
        let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
            continue;
        };
        let request = requests.contains(&d);
        // An argument that may be `c`: `tick`, or a closure capturing it (`setTimeout(tick, 0)`
        // passes the adapter `async () => tick()`).
        let passes1 = |x: &Expr| match x.kind {
            E::Local(l, _) => g
                .ids
                .get(&Node::Local(d, l))
                .is_some_and(|&n| flows(n, lit.as_slice())),
            E::Closure(w) => {
                matches!(&cx.defs[w.0 as usize], Some(Def::Fn(wf)) if wf.captures.iter().any(|k| {
                    g.ids
                        .get(&Node::Local(d, k.outer))
                        .is_some_and(|&n| flows(n, lit.as_slice()))
                }))
            }
            _ => false,
        };
        let passes = |x: &Expr| match &x.kind {
            // The adapter is made in a block (`{ let f = tick; async () => f() }`).
            E::Block(b) => b.value.as_ref().is_some_and(|v| passes1(v)),
            _ => passes1(x),
        };
        each_expr(&f.body.block, &mut |e: &Expr| {
            // `c` passed to the standard library, which calls it.
            if let E::Call {
                callee: Callee::Def(s, _),
                args,
            } = &e.kind
            {
                if in_std(cx, *s) && args.iter().any(passes) {
                    let key = (1, !request, e.span.lo);
                    if let Some(text) = shown(cx, f, e) {
                        if best.as_ref().is_none_or(|b| key < (b.0, b.1, b.2)) {
                            best = Some((1, !request, e.span.lo, text, e.span));
                        }
                    }
                }
                return;
            }
            let E::Call {
                callee: Callee::Indirect(callee),
                ..
            } = &e.kind
            else {
                return;
            };
            let field = match &callee.kind {
                E::Field { base, index, .. } => match cx.ty.kind(base.ty) {
                    TyKind::Adt(a, _) => fields.get(&c).map(|fs| fs.contains(&(*a, *index))),
                    _ => None,
                },
                _ => None,
            };
            let node = |x: &Expr| match root(x) {
                Some(l) => g.ids.get(&Node::Local(d, l)).copied(),
                None => None,
            };
            let rank = match &callee.kind {
                E::Field { base, .. } if field == Some(true) => {
                    if node(base).is_some_and(|n| flows(n, homes)) {
                        0
                    } else {
                        1
                    }
                }
                _ if field == Some(false) => return,
                E::Local(..) if node(callee).is_some_and(|n| flows(n, lit.as_slice())) => 1,
                _ if callee.ty == ty => 2,
                _ if types::may_be(cx, ty, callee.ty) && ret(ty) == ret(callee.ty) => 3,
                _ => return,
            };
            let Some(text) = shown(cx, f, callee) else {
                return;
            };
            let key = (rank, !request, e.span.lo);
            if best.as_ref().is_none_or(|b| key < (b.0, b.1, b.2)) {
                best = Some((rank, !request, e.span.lo, text, e.span));
            }
        });
    }
    best.map(|b| (b.3, b.4))
}

/// `e` as written, for a callee: a variable, a field, an element or a call of one.
fn shown(cx: &Ctx, f: &FnDef, e: &Expr) -> Option<String> {
    Some(match &e.kind {
        E::Local(l, _) => f.body.locals.get(l.0 as usize)?.name.clone(),
        E::Field { base, index, .. } => {
            let b = shown(cx, f, base)?;
            let TyKind::Adt(d, _) = cx.ty.kind(base.ty) else {
                return None;
            };
            let field = cx.adt(*d)?.fields.get(*index as usize)?.name.clone();
            format!("{b}.{field}")
        }
        E::Index { base, index, .. } => {
            let i = match &index.kind {
                E::Lit(Lit::Int(n)) => n.to_string(),
                E::Local(l, _) => f.body.locals.get(l.0 as usize)?.name.clone(),
                _ => "…".into(),
            };
            format!("{}[{i}]", shown(cx, f, base)?)
        }
        E::Call {
            callee: Callee::Def(g, _),
            args,
        } => {
            let name = match &cx.defs[g.0 as usize] {
                Some(Def::Fn(gf)) => gf.name.rsplit("::").next()?.to_string(),
                _ => return None,
            };
            let dots = if args.is_empty() { "" } else { "…" };
            format!("{name}({dots})")
        }
        E::Call {
            callee: Callee::Indirect(inner),
            args,
        } => {
            let dots = if args.is_empty() { "" } else { "…" };
            format!("{}({dots})", shown(cx, f, inner)?)
        }
        E::Cast(x) | E::Upcast(x) | E::UnwrapSome(x, _) => shown(cx, f, x)?,
        _ => return None,
    })
}

/// The initializer of the variable closure `c` captured as `cap`, as written (`""`, `0`):
/// found on its declaration in the function making `c` (or the one making that, for a variable
/// captured on the way). `None` when it is not a literal.
pub(super) fn declaration(cx: &Ctx, g: &Graph, c: DefId, cap: LocalId) -> Option<String> {
    let (mut d, mut l) = (c, cap);
    for _ in 0..64 {
        let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
            return None;
        };
        let outer = f.captures.iter().find(|k| k.inner == l)?.outer;
        let p = *g.parent.get(&d)?;
        let Some(Def::Fn(pf)) = &cx.defs[p.0 as usize] else {
            return None;
        };
        if pf.captures.iter().any(|k| k.inner == outer) {
            (d, l) = (p, outer);
            continue;
        }
        let mut v = Decl(outer, None);
        walk::block(&pf.body.block, &mut v);
        return v.1.flatten();
    }
    None
}

/// Finds the initializer of a local's `let`, as written when it is a literal.
struct Decl(LocalId, Option<Option<String>>);

impl Visit for Decl {
    fn stmt(&mut self, s: &Stmt) {
        if let S::Let { local, init } = &s.kind {
            if *local == self.0 && self.1.is_none() {
                self.1 = Some(init.as_ref().and_then(literal));
            }
        }
    }
}

fn literal(e: &Expr) -> Option<String> {
    Some(match &e.kind {
        E::Lit(Lit::Str(s)) => {
            let mut out = String::from("\"");
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        E::Lit(Lit::Int(n)) => n.to_string(),
        E::Lit(Lit::Float(x)) => {
            let s = x.to_string();
            s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
        }
        E::Lit(Lit::Bool(b)) => b.to_string(),
        E::Lit(Lit::Null) => "null".into(),
        E::Unary {
            op: UnOp::Neg,
            expr,
        } => format!("-{}", literal(expr)?),
        E::ArrayLit(xs) if xs.is_empty() => "[]".into(),
        E::Cast(x) | E::WrapSome(x) | E::Upcast(x) => literal(x)?,
        _ => return None,
    })
}

/// How a variable is shared: as `shared(…)` itself (a 64-bit integer), in a `Mutex` (a value
/// that is copied) or in an object in one.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Int,
    Copy,
    Object,
}

/// The declaration sharing a variable of type `t` initialized with `init` (as written, when a
/// literal): the type is written out where the initializer alone may not give it (`null`, `[]`,
/// an object's field, an initializer that is not a literal).
fn fix(kind: Kind, t: &str, init: Option<&str>) -> String {
    let typed = matches!(init, None | Some("null" | "[]"));
    let init = init.unwrap_or("…");
    match kind {
        Kind::Int => format!("shared({init})"),
        Kind::Copy if typed => format!("shared(new Mutex<{t}>({init}))"),
        Kind::Copy => format!("shared(new Mutex({init}))"),
        Kind::Object => format!("shared(new Mutex<{{ value: {t} }}>({{ value: {init} }}))"),
    }
}

pub(super) fn report(
    cx: &mut Ctx,
    name: &str,
    ty: TyId,
    at: Span,
    call: Option<(String, Span)>,
    init: Option<String>,
    why: Why,
) {
    let mut d = match &call {
        Some((callee, span)) => Diagnostic::error(
            format!("this handler calls `{callee}`, which changes `{name}`; requests run at the same time"),
            *span,
        )
        .with_label(at, format!("`{name}` is changed here")),
        None => Diagnostic::error(
            format!("an HTTP handler can call this function, which changes `{name}`; requests run at the same time"),
            at,
        )
        .with_label(why.span, "the handler is passed to `serve` here"),
    };
    // `shared` alone holds a 64-bit integer (`add`, `get`, `set`); any other value goes in a
    // `Mutex`, and one that is not copied (a string, an object) in an object there, since a
    // `with` callback can replace the fields of what it gets but not the value itself.
    let kind = if matches!(
        cx.ty.kind(ty),
        TyKind::Int(IntTy::I64 | IntTy::U64 | IntTy::ISize | IntTy::USize)
    ) {
        Kind::Int
    } else if cx.is_copy(ty) {
        Kind::Copy
    } else {
        Kind::Object
    };
    let decl = fix(kind, &cx.display(ty), init.as_deref());
    let how = match kind {
        Kind::Int => format!("then read it with `{name}.get()` and change it with `{name}.set(v)` or `{name}.add(1)`"),
        Kind::Copy => format!("then read it with `{name}.with((v) => v)` and change it with `{name}.with((v) => {{ v = … }})`"),
        Kind::Object => format!("then read it with `{name}.with((v) => v.value)` and change it with `{name}.with((v) => {{ v.value = … }})`"),
    };
    d = d
        .with_note(format!(
            "fix: const {name} = {decl}  // one value shared by every request, as in Node"
        ))
        .with_note(how);
    cx.error(d);
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_common::FileId;

    fn e(kind: E) -> Expr {
        Expr {
            kind,
            ty: TyId(0),
            span: Span::new(FileId(0), 0, 0),
        }
    }

    #[test]
    fn literal_initializers_are_shown_as_written() {
        let s = |x: &str| literal(&e(E::Lit(Lit::Str(x.into()))));
        assert_eq!(s("").as_deref(), Some("\"\""));
        assert_eq!(s("a\"b\\").as_deref(), Some("\"a\\\"b\\\\\""));
        assert_eq!(literal(&e(E::Lit(Lit::Int(7)))).as_deref(), Some("7"));
        assert_eq!(literal(&e(E::Lit(Lit::Float(1.5)))).as_deref(), Some("1.5"));
        assert_eq!(literal(&e(E::Lit(Lit::Float(2.0)))).as_deref(), Some("2"));
        let neg = E::Unary {
            op: UnOp::Neg,
            expr: Box::new(e(E::Lit(Lit::Int(3)))),
        };
        assert_eq!(literal(&e(neg)).as_deref(), Some("-3"));
        assert_eq!(literal(&e(E::ArrayLit(vec![]))).as_deref(), Some("[]"));
        let call = E::ArrayLit(vec![e(E::Lit(Lit::Int(1)))]);
        assert_eq!(literal(&e(call)), None);
        assert_eq!(literal(&e(E::Lit(Lit::Null))).as_deref(), Some("null"));
    }

    #[test]
    fn fix_writes_the_type_where_the_initializer_does_not_give_it() {
        assert_eq!(fix(Kind::Int, "i64", Some("0")), "shared(0)");
        assert_eq!(fix(Kind::Copy, "number", Some("0")), "shared(new Mutex(0))");
        assert_eq!(
            fix(Kind::Copy, "number", None),
            "shared(new Mutex<number>(…))"
        );
        assert_eq!(
            fix(Kind::Object, "string", Some("\"\"")),
            "shared(new Mutex<{ value: string }>({ value: \"\" }))"
        );
        assert_eq!(
            fix(Kind::Object, "string | null", Some("null")),
            "shared(new Mutex<{ value: string | null }>({ value: null }))"
        );
        assert_eq!(
            fix(Kind::Object, "string[]", Some("[]")),
            "shared(new Mutex<{ value: string[] }>({ value: [] }))"
        );
    }
}
