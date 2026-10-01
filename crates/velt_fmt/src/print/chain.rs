//! Member chains (prettier's `printMemberChain`): `a.b(x).c(y).d(z)` is split into groups — the
//! head (`a`, plus `this.items`-style property paths) and one group per `.name(args)` link. Short
//! chains print on one line; long ones put every link on its own indented line:
//!
//! ```text
//! items
//!   .filter((x) => x.ok)
//!   .map((x) => x.name);
//! ```

use velt_syntax::ast::{ArrowBody, Expr, ExprKind, ObjectProp};

use super::call::{is_memberish, member_suffix};
use super::Printer;
use crate::doc::{break_parent, cat, concat, conditional, group, hardline, indent, join, nil, Doc};

/// One postfix step of a chain.
#[derive(Clone, Copy)]
enum Link<'e> {
    /// `.prop` / `?.prop` (the expression is the member access).
    Member(&'e Expr),
    /// `(args)` (the expression is the call).
    Call(&'e Expr),
    /// `[index]`.
    Index(&'e Expr),
}

impl Link<'_> {
    fn is_memberish(&self) -> bool {
        matches!(self, Link::Member(_) | Link::Index(_))
    }

    fn is_call(&self) -> bool {
        matches!(self, Link::Call(_))
    }

    /// `[0]`-style access with a literal index (stays attached to what precedes it).
    fn is_literal_index(&self) -> bool {
        match self {
            Link::Index(e) => matches!(
                &e.kind,
                ExprKind::Index { index, .. } if matches!(index.kind, ExprKind::Lit(_))
            ),
            _ => false,
        }
    }
}

impl<'a> Printer<'a> {
    /// `call` is a call whose callee is a member access.
    pub(super) fn member_chain(&mut self, call: &Expr) -> Doc {
        let mut links = vec![];
        let base = flatten(call, &mut links);
        links.reverse();
        let base_doc = self.expr(base);
        let printed: Vec<Doc> = links.iter().map(|l| self.link(*l)).collect();
        let groups = split_groups(base, &links);
        let merge = should_merge(base, &links, &groups);
        let mut docs = vec![base_doc];
        docs.extend(printed);
        let printed_groups: Vec<Doc> = groups
            .iter()
            .map(|g| concat(g.clone().map(|i| docs[i].clone()).collect()))
            .collect();
        let one_line = concat(printed_groups.clone());
        let cutoff = if merge { 3 } else { 2 };
        if printed_groups.len() <= cutoff {
            return group(one_line);
        }
        let split = if merge { 2 } else { 1 };
        let expanded = cat![
            concat(printed_groups[..split].to_vec()),
            indent(group(cat![
                hardline(),
                join(&hardline(), printed_groups[split..].to_vec())
            ]))
        ];
        let calls: Vec<&Expr> = links
            .iter()
            .filter_map(|l| match l {
                Link::Call(e) => Some(*e),
                _ => None,
            })
            .collect();
        let complex_calls = calls.len() > 2 && calls.iter().any(|c| !simple_args(c));
        let early_breaks = printed_groups[..printed_groups.len() - 1]
            .iter()
            .any(Doc::breaks);
        if complex_calls || early_breaks {
            return group(expanded);
        }
        let force = if one_line.breaks() {
            break_parent()
        } else {
            nil()
        };
        cat![force, conditional(vec![one_line, expanded])]
    }

    fn link(&mut self, link: Link) -> Doc {
        match link {
            Link::Member(e) => match &e.kind {
                ExprKind::Member { prop, optional, .. } => member_suffix(prop, *optional),
                _ => nil(),
            },
            Link::Call(e) => match &e.kind {
                ExprKind::Call {
                    type_args,
                    args,
                    optional,
                    ..
                } => self.call_suffix(type_args, args, *optional, e.span.hi),
                _ => nil(),
            },
            Link::Index(e) => match &e.kind {
                ExprKind::Index {
                    index, optional, ..
                } => self.index_suffix(index, *optional),
                _ => nil(),
            },
        }
    }
}

/// Collects the postfix links of `e` (outermost first) and returns the chain's base.
fn flatten<'e>(mut e: &'e Expr, out: &mut Vec<Link<'e>>) -> &'e Expr {
    let mut root = true;
    loop {
        let (link, next) = match &e.kind {
            ExprKind::Call { callee, .. } if root || chains_through(callee) => {
                (Link::Call(e), callee)
            }
            ExprKind::Member { object, .. } => (Link::Member(e), object),
            ExprKind::Index { object, .. } => (Link::Index(e), object),
            _ => return e,
        };
        out.push(link);
        e = next;
        root = false;
    }
}

/// A call continues the chain when its callee is itself part of one (`a.b()`, `a.b()()`).
fn chains_through(callee: &Expr) -> bool {
    is_memberish(callee) || matches!(callee.kind, ExprKind::Call { .. })
}

/// Groups as index ranges into `[base, links...]`: the head, then one group per link that
/// follows a call.
fn split_groups(base: &Expr, links: &[Link]) -> Vec<std::ops::Range<usize>> {
    let mut i = 0;
    while i < links.len() && (links[i].is_call() || links[i].is_literal_index()) {
        i += 1;
    }
    if !matches!(base.kind, ExprKind::Call { .. }) {
        while i + 1 < links.len() && links[i].is_memberish() && links[i + 1].is_memberish() {
            i += 1;
        }
    }
    let mut groups = Vec::new();
    groups.push(0..i + 1);
    let mut start = i + 1;
    let mut seen_call = false;
    for (k, link) in links.iter().enumerate().skip(i) {
        let at = k + 1;
        if seen_call && link.is_memberish() {
            if matches!(link, Link::Index(_)) && !link.is_literal_index() {
                continue;
            }
            groups.push(start..at);
            start = at;
            seen_call = false;
        }
        seen_call |= link.is_call();
    }
    if start < links.len() + 1 {
        groups.push(start..links.len() + 1);
    }
    groups
}

/// Keep the first link on the head's line (`this.x`, `Foo.create()`, `a[0]`): wrapping it alone
/// reads worse.
fn should_merge(base: &Expr, links: &[Link], groups: &[std::ops::Range<usize>]) -> bool {
    let Some(second) = groups.get(1) else {
        return false;
    };
    let computed = links
        .get(second.start - 1)
        .is_some_and(Link::is_literal_index);
    if groups[0].len() == 1 {
        return match &base.kind {
            ExprKind::This => true,
            ExprKind::Ident(id) => is_factory(&id.name) || computed,
            _ => false,
        };
    }
    match links.get(groups[0].end - 2) {
        Some(Link::Member(e)) => match &e.kind {
            ExprKind::Member { prop, .. } => is_factory(&prop.name) || computed,
            _ => false,
        },
        _ => false,
    }
}

/// Capitalized names (`Foo.create()`) or `$`/`_` placeholders.
fn is_factory(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase()) || name.chars().all(|c| c == '$' || c == '_')
}

/// Are all arguments of `call` simple (literals, names, short accesses, callbacks)?
fn simple_args(call: &Expr) -> bool {
    match &call.kind {
        ExprKind::Call { args, .. } => args.iter().all(|a| is_simple(a, 0)),
        _ => true,
    }
}

fn is_simple(e: &Expr, depth: u32) -> bool {
    match &e.kind {
        ExprKind::Lit(_) | ExprKind::Ident(_) | ExprKind::This | ExprKind::Super => true,
        ExprKind::Template { exprs, .. } => exprs.iter().all(|x| is_simple(x, depth)),
        ExprKind::Object(props) | ExprKind::StructLit { props, .. } => {
            props.iter().all(|p| match p {
                ObjectProp::KeyValue(_, v) | ObjectProp::Spread(v) => is_simple(v, depth),
                ObjectProp::Shorthand(_) => true,
            })
        }
        ExprKind::Array(elems) => elems.iter().all(|x| is_simple(x, depth)),
        ExprKind::Arrow { body, .. } => match body {
            ArrowBody::Block(_) => true,
            ArrowBody::Expr(b) => is_simple(b, depth),
        },
        ExprKind::Unary { expr: inner, .. }
        | ExprKind::Paren(inner)
        | ExprKind::Member { object: inner, .. } => is_simple(inner, depth),
        ExprKind::Index { object, index, .. } => {
            is_simple(object, depth) && is_simple(index, depth)
        }
        ExprKind::Call { callee, args, .. } => {
            depth < 2 && is_simple(callee, depth) && args.iter().all(|a| is_simple(a, depth + 1))
        }
        ExprKind::New { args, .. } => depth < 2 && args.iter().all(|a| is_simple(a, depth + 1)),
        _ => false,
    }
}
