//! Scans of a module's syntax for what decides whether the program needs some runtime support.

/// Does `module` call `Object.create(null)`? Then records may have no prototype, which `in`,
/// printing and cloning test at run time (`hir::Program::null_proto_records`); other programs
/// get none of that code.
pub(crate) fn calls_object_create_null(module: &velt_syntax::ast::Module) -> bool {
    use velt_syntax::ast::{self, ExprKind as E};
    struct Find(bool);
    impl<'a> velt_syntax::visit::Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            let E::Call { callee, args, .. } = &e.kind else {
                return;
            };
            let (E::Member { object, prop, .. }, [arg]) = (&callee.kind, args.as_slice()) else {
                return;
            };
            // As `object_create` accepts it: `null` in any parentheses.
            let mut arg = arg;
            while let E::Paren(inner) = &arg.kind {
                arg = inner;
            }
            if prop.name == "create"
                && matches!(&object.kind, E::Ident(o) if o.name == "Object")
                && matches!(arg.kind, E::Lit(ast::Lit::Null))
            {
                self.0 = true;
            }
        }
    }
    let mut f = Find(false);
    velt_syntax::visit::walk_module(module, &mut f);
    f.0
}

/// EXPERIMENT (#2943): does the program's code (its modules and its dependencies', not std's)
/// name `Proxy` or `Reflect`, so that lazy std would load them? A check by units has them
/// loaded with std's unit (`Ctx::units_proxy`, `FnCx::uses_proxy`).
pub(crate) fn program_names_proxy(cx: &crate::ctx::Ctx) -> bool {
    (cx.modules.iter())
        .filter(|m| !m.is_std)
        .any(|m| names_proxy(&m.ast))
}

/// Does `module` name one of std/prelude/global/proxy.velt's globals (`Proxy`, `Reflect`,
/// `ProxyHandler`, `PropertyDescriptor`, as a value or a type), or put a literal nothing where
/// its type may have none ([`has_nothing_slot`])? The loader loads `Proxy` for such a module
/// (velt_loader `globals::names_global`); only a program that has one has proxies
/// (`body::expr::proxy`).
pub(crate) fn names_proxy(module: &velt_syntax::ast::Module) -> bool {
    use velt_syntax::ast::{self, ExprKind as E};
    const NAMES: [&str; 4] = ["Proxy", "Reflect", "ProxyHandler", "PropertyDescriptor"];
    struct Find(bool);
    impl<'a> velt_syntax::visit::Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            if let E::Ident(id) = &e.kind {
                self.0 |= NAMES.contains(&id.name.as_str());
            }
        }
        fn ty(&mut self, t: &'a ast::TypeExpr) {
            if let ast::TypeExprKind::Named { path, .. } = &t.kind {
                self.0 |= path
                    .first()
                    .is_some_and(|p| NAMES.contains(&p.name.as_str()));
            }
        }
    }
    let mut f = Find(false);
    velt_syntax::visit::walk_module(module, &mut f);
    f.0 || has_nothing_slot(module)
}

/// Whether `module` asserts a literal `null` or `undefined` non-null (`null!`) or casts one to a
/// type written without a nothing (`null as any`, `null as Foo`; not `null as Foo | null`): a
/// view of the nothing holds it (velt_vir `lower/any_view.rs`), so the loader loads `Proxy` for
/// it (velt_loader `globals::names_global`).
pub fn has_nothing_slot(module: &velt_syntax::ast::Module) -> bool {
    use velt_syntax::ast;
    fn nothing(e: &ast::Expr) -> bool {
        match &e.kind {
            ast::ExprKind::Paren(inner) => nothing(inner),
            ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined) => true,
            _ => false,
        }
    }
    fn names_nothing(t: &ast::TypeExpr) -> bool {
        match &t.kind {
            ast::TypeExprKind::Union(parts) => parts.iter().any(names_nothing),
            ast::TypeExprKind::Null | ast::TypeExprKind::Undefined => true,
            _ => false,
        }
    }
    struct Find(bool);
    impl<'a> velt_syntax::visit::Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            match &e.kind {
                ast::ExprKind::NonNull(inner) => self.0 |= nothing(inner),
                ast::ExprKind::Cast { expr, ty } => self.0 |= nothing(expr) && !names_nothing(ty),
                _ => {}
            }
        }
    }
    let mut find = Find(false);
    velt_syntax::visit::walk_module(module, &mut find);
    find.0
}

/// Does `module` read a `constructor` property (`x.constructor`)? Only then does the prelude's
/// `anyConstructor` hold the dispatch over the program's classes, whose objects are module
/// state (`body::expr::ctor_objects`); other programs get none of it.
pub(crate) fn reads_constructor(module: &velt_syntax::ast::Module) -> bool {
    use velt_syntax::ast::{self, ExprKind as E};
    struct Find(bool);
    impl<'a> velt_syntax::visit::Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            if matches!(&e.kind, E::Member { prop, .. } if prop.name == "constructor") {
                self.0 = true;
            }
        }
    }
    let mut f = Find(false);
    velt_syntax::visit::walk_module(module, &mut f);
    f.0
}

/// What [`program`] found in the program's own modules (std is left out).
pub(crate) struct Found {
    /// Some module calls `Object.create(null)` ([`calls_object_create_null`]).
    pub null_proto: bool,
    /// Some module reads a `constructor` ([`reads_constructor`]).
    pub ctor_reads: bool,
    /// Some module defines accessors (`crate::defined_accessors::defines_accessors`).
    pub record_accessors: bool,
    /// Some module calls a `throw()` method (`crate::ts_protocol::calls_throw`).
    pub gen_throw: bool,
}

/// Scan the program's own modules once for every round of the check, each module on its own
/// (`crate::par`).
pub(crate) fn program(modules: &[crate::SourceModule]) -> Found {
    // Std is not scanned: neither weighed nor mapped, so a small program starts no threads.
    let own: Vec<&crate::SourceModule> = modules.iter().filter(|m| !m.is_std).collect();
    let weight = crate::par::module_weight(own.iter().copied());
    let scans = crate::par::map(&own, weight, |m| {
        [
            calls_object_create_null(&m.ast),
            reads_constructor(&m.ast),
            crate::defined_accessors::defines_accessors(&m.ast),
            crate::ts_protocol::calls_throw(std::slice::from_ref(*m)),
        ]
    });
    let any = |k: usize| scans.iter().any(|s| s[k]);
    Found {
        null_proto: any(0),
        ctor_reads: any(1),
        record_accessors: any(2),
        gen_throw: any(3),
    }
}
