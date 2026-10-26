//! Types as diagnostics write them: `Ctx::display` and `Ctx::display_in`.

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{AdtKind, TyId, TyKind};
use crate::types::{display_key, int_name};

impl Ctx<'_> {
    /// Human-readable type for diagnostics.
    pub fn display(&self, t: TyId) -> String {
        self.display_in(t, &self.display_params)
    }

    /// Is `t` an error-set variable, or a union of them (what two sites' variables join to)?
    /// Shown as written: nothing (`throws::vars`).
    fn only_err_vars(&self, t: TyId) -> bool {
        if self.is_err_var(t) {
            return true;
        }
        let Some((d, args)) = self.union_def(t) else {
            return false;
        };
        let Some(e) = self.enum_info(d) else {
            return false;
        };
        args.is_empty()
            && e.variants
                .iter()
                .all(|v| v.payload.first().is_some_and(|p| self.is_err_var(*p)))
    }

    /// Display `t` with `Param(i)` named `names[i]`.
    pub(crate) fn display_in(&self, t: TyId, names: &[String]) -> String {
        let list = |s: &Self, ts: &[TyId]| {
            ts.iter()
                .map(|t| s.display_in(*t, names))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let with_args = |s: &Self, name: &str, args: &[TyId]| {
            let name = crate::namespaces::written(name);
            if args.is_empty() {
                name.into_owned()
            } else {
                format!("{name}<{}>", list(s, args))
            }
        };
        if let Some(n) = self.alias_names.get(&t) {
            return crate::namespaces::written(n).into_owned();
        }
        match self.ty.kind(t) {
            TyKind::Int(i) => int_name(*i).to_string(),
            TyKind::Float(crate::hir::FloatTy::F32) => "f32".into(),
            TyKind::Float(crate::hir::FloatTy::F64) => "f64".into(),
            TyKind::Bool => "boolean".into(),
            TyKind::Str => "string".into(),
            TyKind::Symbol => "symbol".into(),
            TyKind::Simd(s) => s.name().into(),
            TyKind::Unit => "void".into(),
            TyKind::Never => "never".into(),
            TyKind::Error => "_".into(),
            TyKind::Literal(v) => crate::literals::display_lit(v),
            TyKind::Array(e) => match self.ty.kind(*e) {
                TyKind::Option(..) | TyKind::FnPtr { .. } => {
                    format!("({})[]", self.display_in(*e, names))
                }
                _ if self.union_def(*e).is_some() => format!("({})[]", self.display_in(*e, names)),
                _ => format!("{}[]", self.display_in(*e, names)),
            },
            TyKind::Map(k, v) => format!(
                "Map<{}, {}>",
                self.display_in(*k, names),
                self.display_in(*v, names)
            ),
            TyKind::Tuple(ts) => format!("[{}]", list(self, ts)),
            // `undefined` alone is an option of `never` (`Types::undefined`).
            TyKind::Option(t, n) if *t == self.ty.never => n.suffix()[3..].to_string(),
            // `(() => T) | undefined`, not the function type `() => T | undefined`.
            TyKind::Option(t, n) if matches!(self.ty.kind(*t), TyKind::FnPtr { .. }) => {
                format!("({}){}", self.display_in(*t, names), n.suffix())
            }
            TyKind::Option(t, n) => format!("{}{}", self.display_in(*t, names), n.suffix()),
            TyKind::Result(a, b) => with_args(self, "Result", &[*a, *b]),
            // A variable is shown as written: nothing (`throws::vars`).
            TyKind::Promise(t, e) if *e == self.ty.never || self.only_err_vars(*e) => {
                with_args(self, "Promise", &[*t])
            }
            TyKind::Promise(t, e) => with_args(self, "Promise", &[*t, *e]),
            TyKind::Shared(t) => with_args(self, "shared", &[*t]),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => {
                let mut ps: Vec<String> =
                    params.iter().map(|p| self.display_in(*p, names)).collect();
                if let (true, Some(last)) = (*rest, ps.last_mut()) {
                    last.insert_str(0, "...");
                }
                // A function type written without `throws`, as written (`throws::fn_types`).
                let promise_var =
                    matches!(self.ty.kind(*ret), TyKind::Promise(_, e) if self.only_err_vars(*e));
                if self.ty.inferred_fns.contains(&t) || self.only_err_vars(*throws) || promise_var {
                    let ret = match self.ty.kind(*ret) {
                        TyKind::Promise(v, _) => {
                            format!("Promise<{}>", self.display_in(*v, names))
                        }
                        _ => self.display_in(*ret, names),
                    };
                    return format!("({}) => {ret}", ps.join(", "));
                }
                let th = if *throws == self.ty.never {
                    String::new()
                } else {
                    format!(" throws {}", self.display_in(*throws, names))
                };
                format!(
                    "({}) => {}{th}",
                    ps.join(", "),
                    self.display_in(*ret, names)
                )
            }
            TyKind::Adt(d, args) => match &self.info[d.0 as usize] {
                DefInfo::Adt(_) if self.ctor_sig_own(t).is_some() => self.display_ctor(t, names),
                DefInfo::Adt(a) if a.kind == AdtKind::Anon => {
                    // Field types are written over the anon def's own parameters, which
                    // `args` binds to the parameters in scope.
                    let bound: Vec<String> =
                        args.iter().map(|t| self.display_in(*t, names)).collect();
                    let fs: Vec<String> = a
                        .fields
                        .iter()
                        .map(|f| {
                            // A function with properties shows its call signature (#921).
                            if f.name == crate::callables::CALL_FIELD {
                                if let TyKind::FnPtr { params, ret, .. } = self.ty.kind(f.ty) {
                                    let ps: Vec<String> = params
                                        .iter()
                                        .map(|p| self.display_in(*p, &bound))
                                        .collect();
                                    let ret = self.display_in(*ret, &bound);
                                    return format!("({}): {ret}", ps.join(", "));
                                }
                            }
                            let name = self
                                .key_spelling(&f.name)
                                .unwrap_or_else(|| display_key(&f.name));
                            if f.optional {
                                format!("{name}?: {}", self.display_in(f.declared, &bound))
                            } else {
                                format!("{name}: {}", self.display_in(f.ty, &bound))
                            }
                        })
                        .collect();
                    format!("{{ {} }}", fs.join("; "))
                }
                DefInfo::Adt(a) => with_args(self, &a.name, self.shown_args(*d, args)),
                DefInfo::Enum(e) if e.is_union => self.display_union(*d, args, names),
                DefInfo::Enum(e) => with_args(self, &e.name, args),
                _ => "unknown".into(),
            },
            TyKind::Dyn(d, args) if self.literal_ifaces.contains(d) => {
                self.display_literal_iface(*d, args, names)
            }
            TyKind::Dyn(d, args) if self.synth_display(*d, args, names).is_some() => {
                self.synth_display(*d, args, names).unwrap_or_default()
            }
            TyKind::Dyn(d, args) => match self.iface(*d) {
                Some(i) => with_args(self, &i.name, args),
                None => "unknown".into(),
            },
            TyKind::Op(o, args) => self.display_op(*o, args, names),
            TyKind::Closure(d) => match self.try_fn(*d) {
                Some(f) => {
                    let ps: Vec<String> = f
                        .params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, self.display_in(p.ty, names)))
                        .collect();
                    format!("({}) => {}", ps.join(", "), self.display_in(f.ret, names))
                }
                None => "unknown".into(),
            },
            TyKind::Param(n) => names
                .get(*n as usize)
                .cloned()
                .unwrap_or_else(|| format!("T{n}")),
        }
    }
}

impl Ctx<'_> {
    /// The interface of an object literal with no expected type (`body/expr/literal_iface.rs`)
    /// as TypeScript shows the literal's type, its members in written order:
    /// `{ n: number; get d(): number; m(k: number): number }`. A result not known yet (while
    /// the literal's methods are checked) shows as `any`, as in TypeScript.
    fn display_literal_iface(
        &self,
        d: crate::hir::DefId,
        args: &[TyId],
        names: &[String],
    ) -> String {
        let Some(i) = self.iface(d) else {
            return "unknown".into();
        };
        if self.literal_display.borrow().contains(&d) {
            return "this".into();
        }
        self.literal_display.borrow_mut().push(d);
        // The interface is over the generic parameters in scope where the literal is written.
        let own: Vec<String> = args.iter().map(|a| self.display_in(*a, names)).collect();
        let ty = |t: TyId| match self.ty.has_error(t) {
            true => "any".to_string(),
            false => self.display_in(t, &own),
        };
        let key = |n: &str| self.key_spelling(n).unwrap_or_else(|| display_key(n));
        let mut parts: Vec<(u32, String)> = vec![];
        for f in &i.fields {
            parts.push((f.span.lo, format!("{}: {}", key(&f.name), ty(f.ty))));
        }
        for m in &i.methods {
            let params: Vec<String> = m
                .params
                .iter()
                .map(|p| {
                    // The setter `Object.defineProperty` adds (`crate::defined_accessors`).
                    let name = match p.name == crate::defined_accessors::ADDED_SETTER_PARAM {
                        true => "v",
                        false => &p.name,
                    };
                    format!("{name}: {}", ty(p.ty))
                })
                .collect();
            let s = match (m.is_getter, m.name.strip_prefix("set ")) {
                (true, _) => format!("get {}(): {}", key(&m.name), ty(m.ret)),
                (_, Some(n)) => format!("set {}({})", key(n), params.join(", ")),
                _ => format!("{}({}): {}", key(&m.name), params.join(", "), ty(m.ret)),
            };
            parts.push((m.span.lo, s));
        }
        self.literal_display.borrow_mut().pop();
        parts.sort_by_key(|(lo, _)| *lo);
        let parts: Vec<String> = parts.into_iter().map(|(_, s)| s).collect();
        format!("{{ {} }}", parts.join("; "))
    }
}
