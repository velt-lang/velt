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
        if let Some(facts) = self.generated_arm_facts(callee, args) {
            return facts;
        }
        let (mut t, f) = match self.dynamic_is_array_facts(callee, args) {
            Some(facts) => facts,
            None => match self.is_array_test(callee, args) {
                Some(arg) => self.is_array_facts(arg),
                None => self.predicate_facts(callee, args, cond.span),
            },
        };
        t.extend(self.chain_facts(cond));
        t.extend(self.has_key_facts(callee, args));
        (t, f)
    }

    /// `r.__has(k)` (the test `for (const k in r)` starts its body with, `object_keys`): `r[k]`
    /// is present (`key_narrow`), so not nullish when the record's values never are.
    fn has_key_facts(&mut self, callee: &ast::Expr, args: &[ast::Expr]) -> Vec<Fact> {
        let (
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            },
            [k],
        ) = (&callee.kind, args)
        else {
            return vec![];
        };
        if prop.name != "__has" || self.key_prop(k).is_none() {
            return vec![];
        }
        // A present key of a `Record<string, V | null>` may still hold `null`.
        let value = self.peek_ty(object).and_then(|t| self.record_args(t));
        if !value.is_some_and(|(_, v)| self.cx.ty.nothing_of(v).is_none()) {
            return vec![];
        }
        let read = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Index {
                object: object.clone(),
                index: Box::new(k.clone()),
                optional: false,
            },
            span: callee.span,
        };
        self.field_token(&read)
            .map(Fact::NonNull)
            .into_iter()
            .collect()
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

impl FnCx<'_, '_> {
    /// The facts of `__arm(v, i)` in generated code (`generated::arm`) on a union local `v`:
    /// member `i` where it holds, the others where it does not.
    fn generated_arm_facts(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let (v, i) = self.arm_call(callee, args)?;
        let (l, _, u) = self.local_with_members(v)?;
        let n = self.cx.union_members(u)?.len() as u32;
        let others = (0..n).filter(|k| *k != i).collect();
        Some((
            vec![Fact::Members(l, vec![i])],
            vec![Fact::Members(l, others)],
        ))
    }
}
