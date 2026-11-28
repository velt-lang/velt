//! `performance.now()` is the compiler's own clock read (sema's `Intrinsic::PerfNow`): a module
//! that uses `performance` only that way doesn't load the `performance` global
//! (std/prelude/global/performance.velt), whose marks are module state.

use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

/// Whether `module` uses `performance` for more than `performance.now`: reads it otherwise
/// (`performance.mark(…)`, `const p = performance`, `{ performance }`, `globalThis.performance`)
/// or names it in a type (`typeof performance`).
pub(super) fn beyond_now(module: &ast::Module) -> bool {
    #[derive(Default)]
    struct Find {
        reads: usize,
        now: usize,
        other: bool,
    }
    impl<'a> Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            match &e.kind {
                ast::ExprKind::Ident(id) if id.name == "performance" => self.reads += 1,
                ast::ExprKind::Member { object, prop, .. } => match &object.kind {
                    ast::ExprKind::Ident(id) if id.name == "performance" => {
                        self.now += usize::from(prop.name == "now");
                    }
                    _ => self.other |= prop.name == "performance",
                },
                ast::ExprKind::Object(props) => {
                    self.other |= props.iter().any(
                        |p| matches!(p, ast::ObjectProp::Shorthand(id) if id.name == "performance"),
                    );
                }
                _ => {}
            }
        }
        fn ty(&mut self, t: &'a ast::TypeExpr) {
            if let ast::TypeExprKind::Query(path) = &t.kind {
                self.other |= path.first().is_some_and(|p| p.name == "performance");
            }
        }
    }
    let mut find = Find::default();
    visit::walk_module(module, &mut find);
    find.other || find.reads > find.now
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use velt_common::SourceMap;

    fn beyond(src: &str) -> bool {
        let mut sm = SourceMap::new();
        let file = sm.add(Path::new("m.ts"), src.to_string());
        let (module, _) = velt_syntax::parse_file(file, &sm.get(file).src);
        beyond_now(&module)
    }

    #[test]
    fn now_alone_is_the_compilers() {
        assert!(!beyond(
            "const t = performance.now();\nconsole.log(performance.now() - t);\n"
        ));
        assert!(!beyond("const f = () => performance?.now();\n"));
    }

    #[test]
    fn other_uses_load_the_global() {
        assert!(beyond("performance.now();\nperformance.mark(\"a\");\n"));
        assert!(beyond("const p = performance;\n"));
        assert!(beyond("const { now } = performance;\n"));
        assert!(beyond("const o = { performance };\n"));
        assert!(beyond("const t = globalThis.performance.now();\n"));
        assert!(beyond("let p: typeof performance;\n"));
        assert!(beyond("console.log(performance.timeOrigin);\n"));
    }
}
