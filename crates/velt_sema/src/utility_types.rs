//! `Partial<T>`, `Required<T>`, `Readonly<T>`, `Pick<T, K>` and `Omit<T, K>`: built-in type
//! operators on a concrete object type (docs/internals/design/shared-models.md, step 3). Each
//! result is an ordinary anonymous object type, so `Pick<User, "name">` and `{ name: string }`
//! are the same type.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{DefId, LitValue, TyId, TyKind};
use crate::resolve::TyEnv;

/// The operators, by name (a user type of the same name wins, as for every built-in).
pub(crate) const OPERATORS: [&str; 5] = ["Partial", "Required", "Readonly", "Pick", "Omit"];

impl Ctx<'_> {
    /// The type `name<args>` for one of [`OPERATORS`].
    pub(crate) fn resolve_utility(
        &mut self,
        t: &ast::TypeExpr,
        name: &str,
        args: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> TyId {
        let want = if matches!(name, "Pick" | "Omit") {
            2
        } else {
            1
        };
        let tys: Vec<TyId> = args
            .iter()
            .map(|a| {
                let t = self.resolve_type(a, env);
                self.ty.subst(t, &env.args)
            })
            .collect();
        if tys.len() != want {
            self.err(
                format!(
                    "type `{name}` takes {want} type argument(s) but {} were supplied",
                    tys.len()
                ),
                t.span,
            );
            return self.ty.error;
        }
        if tys.contains(&self.ty.error) {
            return self.ty.error;
        }
        let written = match &args[0].kind {
            ast::TypeExprKind::Named { path, .. } if path.len() == 1 => Some(path[0].name.clone()),
            _ => None,
        };
        let Some(mut fields) = self.object_fields(name, tys[0], written, args[0].span) else {
            return self.ty.error;
        };
        match name {
            "Partial" => {
                for f in &mut fields {
                    if self.ty.opt_payload(f.1).is_none() {
                        f.1 = self.ty.option(f.1);
                    }
                }
            }
            "Required" => {
                for f in &mut fields {
                    f.1 = self.ty.opt_payload(f.1).unwrap_or(f.1);
                }
            }
            "Readonly" => fields.iter_mut().for_each(|f| f.2 = true),
            _ => {
                let Some(keys) = self.utility_keys(name, tys[0], &fields, tys[1], args[1].span)
                else {
                    return self.ty.error;
                };
                let pick = name == "Pick";
                fields.retain(|f| keys.contains(&f.0) == pick);
            }
        }
        self.anon_type_with(&fields, env.module)
    }

    /// The public fields of object type `t` (anonymous, a field-only interface, a struct or a
    /// class), with `t`'s type arguments substituted.
    fn object_fields(
        &mut self,
        op: &str,
        t: TyId,
        written: Option<String>,
        span: Span,
    ) -> Option<Vec<(String, TyId, bool)>> {
        let shown = self.display(t);
        match self.ty.kind(t).clone() {
            TyKind::Adt(d, args) if self.adt(d).is_some() => {
                let fields = match self.fields_now(d) {
                    Ok(fields) => fields,
                    Err(cycle) => {
                        let name = match (self.adt(cycle), self.iface(cycle)) {
                            (Some(a), _) => a.name.clone(),
                            (_, Some(i)) => i.name.clone(),
                            _ => shown.clone(),
                        };
                        self.error(
                            Diagnostic::error(
                                format!("`{op}<{shown}>` needs the fields of `{name}` while they are being resolved"),
                                span,
                            )
                            .with_note(format!(
                                "`{name}` refers to itself through a utility type in a field; TypeScript allows this, but Velt doesn't yet: write the fields out"
                            )),
                        );
                        return None;
                    }
                };
                Some(
                    fields
                        .into_iter()
                        .filter(|f| f.3)
                        .map(|(n, ty, r, _)| (n, self.subst(ty, &args), r))
                        .collect(),
                )
            }
            TyKind::Param(_) => {
                let shown = written.unwrap_or(shown);
                self.error(
                    Diagnostic::error(
                        format!(
                            "`{op}` needs a concrete object type; `{shown}` is a type parameter"
                        ),
                        span,
                    )
                    .with_note("utility types on type parameters are not supported yet (#350)"),
                );
                None
            }
            _ => {
                self.err(
                    format!("`{op}` needs an object type; found `{shown}`"),
                    span,
                );
                None
            }
        }
    }

    /// Object type `d`'s fields as (name, type, readonly, public). While declarations are
    /// shaped, `d` (and what it inherits from) is shaped first, whatever the declaration order;
    /// `Err` names a type whose fields are needed while they are being resolved. A field-only
    /// interface's object type is filled after interfaces are flattened, so until then its
    /// fields come from the interface and the ones it extends (inherited first, as
    /// `collect::field_only` orders them).
    fn fields_now(&mut self, d: DefId) -> Result<Vec<(String, TyId, bool, bool)>, DefId> {
        crate::collect::shapes::ensure_fields(self, d)?;
        let a = self.adt(d).expect("ICE: adt");
        if let Some(&iface) = self.field_only_of.get(&d) {
            if a.fields.is_empty() {
                return Ok(self.iface_fields_now(iface, &[], &mut vec![]));
            }
        }
        Ok(a.fields
            .iter()
            .map(|f| (f.name.clone(), f.ty, f.readonly, f.private_to.is_none()))
            .collect())
    }

    /// Interface `iface`'s fields with `args` substituted, inherited ones first, before
    /// inheritance is flattened (the interfaces are shaped: `ensure_fields`).
    fn iface_fields_now(
        &mut self,
        iface: DefId,
        args: &[TyId],
        stack: &mut Vec<DefId>,
    ) -> Vec<(String, TyId, bool, bool)> {
        let Some(i) = self.iface(iface).filter(|_| !stack.contains(&iface)) else {
            return vec![]; // a cycle is reported by `collect::iface_extends`
        };
        let declared = i.decl.map_or(0, |d| d.fields.len());
        let parents = i.parents.clone();
        let own: Vec<(String, TyId, bool)> = i
            .fields
            .iter()
            .take(declared)
            .map(|f| (f.name.clone(), f.ty, f.readonly))
            .collect();
        stack.push(iface);
        let mut out = vec![];
        for p in parents {
            let pargs: Vec<TyId> = p.args.iter().map(|t| self.subst(*t, args)).collect();
            for f in self.iface_fields_now(p.iface, &pargs, stack) {
                if !out.iter().any(|g: &(String, TyId, bool, bool)| g.0 == f.0) {
                    out.push(f);
                }
            }
        }
        stack.pop();
        for (n, ty, r) in own {
            let ty = self.subst(ty, args);
            out.retain(|g| g.0 != n);
            out.push((n, ty, r, true));
        }
        out
    }

    /// The field names `k` lists for `Pick` / `Omit` (a string literal type or a union of them).
    /// In `Pick` each must be a field of `t`; `Omit` accepts other names, as TypeScript does
    /// (`Omit<P, "children">` in an alias used on types without `children`), with a warning.
    fn utility_keys(
        &mut self,
        op: &str,
        t: TyId,
        fields: &[(String, TyId, bool)],
        k: TyId,
        span: Span,
    ) -> Option<Vec<String>> {
        let members = self.union_members(k).unwrap_or_else(|| vec![k]);
        let keys: Option<Vec<String>> = members
            .iter()
            .map(|m| match self.ty.kind(*m) {
                TyKind::Literal(LitValue::Str(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        let Some(keys) = keys else {
            let shown = self.display(k);
            self.error(
                Diagnostic::error(
                    format!(
                        "`{op}` takes the field names as a string literal type; found `{shown}`"
                    ),
                    span,
                )
                .with_note("write them like `\"id\" | \"email\"`"),
            );
            return None;
        };
        let names: Vec<String> = fields.iter().map(|f| f.0.clone()).collect();
        let mut ok = true;
        for key in &keys {
            if names.contains(key) {
                continue;
            }
            let shown = self.display(t);
            let mut d =
                Diagnostic::error(format!("`{shown}` has no field `{key}` (in `{op}`)"), span);
            if op == "Omit" {
                d.severity = velt_common::Severity::Warning;
                d = d.with_note("there is nothing to omit; TypeScript accepts this too");
            } else {
                ok = false;
            }
            if let Some(s) = crate::suggest::closest(key, &names) {
                d = d.with_note(format!("did you mean `{s}`?"));
            }
            self.error(d);
        }
        ok.then_some(keys)
    }
}
