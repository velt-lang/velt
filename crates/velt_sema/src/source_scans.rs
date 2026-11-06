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
