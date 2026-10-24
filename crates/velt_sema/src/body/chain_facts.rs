//! Narrowing by a truthy optional chain (`body/narrow.rs` `truthy_facts`).

use velt_syntax::ast;

use super::narrow::Fact;
use super::FnCx;

impl FnCx<'_, '_> {
    /// Facts of a call used as a condition: `Array.isArray(x)`, a type predicate
    /// (`isFoo(x)` of a function declared `(v: …): v is Foo`), and, when it is true, those of
    /// the optional chain it ends (`c?.m.has(k)`).
    pub(super) fn call_facts(
        &mut self,
        cond: &ast::Expr,
        callee: &ast::Expr,
        args: &[ast::Expr],
    ) -> (Vec<Fact>, Vec<Fact>) {
        let (mut t, f) = match self.dynamic_is_array_facts(callee, args) {
            Some(facts) => facts,
            None => match self.is_array_test(callee, args) {
                Some(arg) => self.is_array_facts(arg),
                None => self.predicate_facts(callee, args, cond.span),
            },
        };
        t.extend(self.chain_facts(cond));
        t.extend(self.present_key_fact(cond.span, callee, args));
        (t, f)
    }

    /// `r.__has(k)` that `for (const k in r)` tests (`object_keys`): when true, `r[k]` is
    /// present, so it reads as a `V` (checked again where it is read, as after `if (r[k])`).
    fn present_key_fact(
        &mut self,
        span: velt_common::Span,
        callee: &ast::Expr,
        args: &[ast::Expr],
    ) -> Option<Fact> {
        let (ast::ExprKind::Member { object, prop, .. }, [key]) = (&callee.kind, args) else {
            return None;
        };
        if prop.name != "__has" || !self.present_key_tests.contains(&span) {
            return None;
        }
        let read = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Index {
                object: object.clone(),
                index: Box::new(key.clone()),
                optional: false,
            },
            span,
        };
        self.field_token(&read).map(Fact::NonNull)
    }

    /// Facts of a truthy optional chain that goes on after its `?.` (`if (c?.m.has(k))`,
    /// `if (o?.f())`): it did not stop there, so the object of each `?.` is not null.
    pub(super) fn chain_facts(&mut self, e: &ast::Expr) -> Vec<Fact> {
        let mut facts = vec![];
        let mut cur = e;
        loop {
            let (object, optional) = match &cur.kind {
                ast::ExprKind::Call {
                    callee, optional, ..
                } => (callee, *optional),
                ast::ExprKind::Member {
                    object, optional, ..
                }
                | ast::ExprKind::Index {
                    object, optional, ..
                } => (object, *optional),
                _ => break,
            };
            if optional {
                facts.extend(self.truthy_facts(object));
            }
            cur = object;
        }
        facts
    }
}
