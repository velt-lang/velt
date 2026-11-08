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
