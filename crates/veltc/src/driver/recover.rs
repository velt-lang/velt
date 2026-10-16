//! `velt check` past syntax errors (#1155): the modules without any are type-checked too.

use std::time::Instant;

use super::{load_modules, sema_stage, BuildError, BuildOptions, CheckScope, Session};

/// What `velt check` runs: [`super::check_program`], except that syntax errors don't stop
/// it. The modules without any go on to the type checker with the recovered rest
/// ([`velt_sema::check_recovered`]), and their errors are reported after the syntax errors: a
/// module importing one with a syntax error still gets its own errors reported, as `tsc` does.
/// The modules with syntax errors report only those (their syntax trees are partial, so the
/// type checker's findings there would be noise), and so does every program when the type
/// checker fails on the recovered trees. Builds still stop after the first stage with errors.
pub(super) fn check_recovering(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<(), BuildError> {
    let loaded = load_modules(sess, opts, scope)?;
    if !sess.has_errors() {
        return sema_stage(sess, opts, scope, &loaded).map(drop);
    }
    let t = Instant::now();
    let broken: std::collections::HashSet<velt_common::FileId> = sess
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .filter_map(primary_span)
        .map(|span| span.file)
        .collect();
    let recovered: Vec<(usize, &str)> = (loaded.modules.iter().enumerate())
        .filter(|(_, m)| broken.contains(&m.file))
        .map(|(i, m)| (i, sess.sm.get(m.file).src.as_str()))
        .collect();
    let sema_opts = velt_sema::CheckOptions {
        require_main: scope.require_main,
    };
    // The recovered trees can hold shapes no clean parse produces: a type checker failure on
    // them costs these findings only, never the syntax errors already reported.
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        velt_sema::check_recovered(&loaded.modules, loaded.root, sema_opts, &recovered)
    }));
    if let Ok(diags) = checked {
        let found = (diags.into_iter())
            .filter(|d| primary_span(d).is_none_or(|span| !broken.contains(&span.file)));
        sess.diagnostics.extend(found);
    }
    sess.record("sema", t);
    Err(BuildError::Diagnostics)
}

/// Where diagnostic `d` is reported (`None`: nowhere in particular).
fn primary_span(d: &velt_common::Diagnostic) -> Option<velt_common::Span> {
    let span = d.labels.first()?.span;
    (span != velt_common::Span::DUMMY).then_some(span)
}

#[cfg(test)]
mod tests {
    use super::super::*;

    /// `velt check` of the files `files` (name, source; the first is the root): each error as
    /// `file: message`, one per line.
    fn check_files(files: &[(&str, &str)]) -> String {
        let dir = tempfile::tempdir().unwrap();
        for (name, src) in files {
            std::fs::write(dir.path().join(name), src).unwrap();
        }
        let opts = BuildOptions {
            input: dir.path().join(files[0].0),
            ..Default::default()
        };
        let mut sess = Session::new();
        let r = check(&mut sess, &opts);
        assert!(matches!(r, Err(BuildError::Diagnostics)), "{r:?}");
        let mut out = String::new();
        for d in sess.diagnostics.iter().filter(|d| d.is_error()) {
            let span = d.labels[0].span;
            let path = &sess.sm.get(span.file).path;
            let file = path.file_name().unwrap().to_string_lossy();
            out.push_str(&format!("{file}: {}\n", d.message));
        }
        out
    }

    const BROKEN: &str = "export function g(): number { return 1; }\n\
        export function dropped( { return 1; }\n\
        export function late(): number { const x: string = 1; return 2; }\n";

    #[test]
    fn check_reports_the_errors_of_modules_importing_one_with_syntax_errors() {
        // `a` imports `b`, whose syntax error drops `dropped`: `a` still reports its own errors
        // (and nothing about `dropped`); `b` reports only its syntax error (#1155).
        let a = "import { g, dropped } from './b';\n\
            import * as B from './b';\n\
            export function h(): number { const s: string = g(); return 2; }\n\
            export function k(): number { return dropped() + B.dropped(); }\n\
            export function m(): boolean { return 1; }\n";
        let out = check_files(&[("a.ts", a), ("b.ts", BROKEN)]);
        assert_eq!(
            out,
            "b.ts: expected identifier, found `{`\n\
             a.ts: mismatched types\n\
             a.ts: mismatched types\n",
        );
    }

    #[test]
    fn check_reports_errors_through_a_module_reexporting_one_with_syntax_errors() {
        let a = "import { dropped, g } from './c';\n\
            export function k(): number { return dropped() + g(); }\n\
            export function m(): boolean { return 1; }\n";
        let c = "export { g, dropped } from './b';\n";
        let out = check_files(&[("a.ts", a), ("c.ts", c), ("b.ts", BROKEN)]);
        assert_eq!(
            out,
            "b.ts: expected identifier, found `{`\na.ts: mismatched types\n"
        );
    }

    #[test]
    fn check_reports_names_a_module_with_syntax_errors_never_mentions() {
        // `nope` appears nowhere in `b`: no syntax error dropped it, so importing it is an error.
        let a = "import { dropped, nope } from './c';\n\
            export function k(): number { return dropped() + nope(); }\n";
        let c = "export * from './b';\n";
        let out = check_files(&[("a.ts", a), ("c.ts", c), ("b.ts", BROKEN)]);
        assert_eq!(
            out,
            "b.ts: expected identifier, found `{`\n\
             a.ts: module `./c` has no member `nope`\n\
             a.ts: cannot find `nope` in this scope\n"
        );
    }

    /// `velt check` of `main` importing `lib` (with syntax errors): the errors after `lib`'s
    /// syntax errors, which must be `syntax` of them.
    fn importer_errors(main: &str, lib: &str, syntax: usize) -> String {
        let out = check_files(&[("main.ts", main), ("lib.ts", lib)]);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines.len() >= syntax && lines[..syntax].iter().all(|l| l.starts_with("lib.ts: ")),
            "{out}"
        );
        lines[syntax..].iter().map(|l| format!("{l}\n")).collect()
    }

    /// Each importer below ends with a type error of its own, which must still be reported.
    const OWN: &str = "main.ts: mismatched types\n";

    #[test]
    fn check_says_nothing_about_a_class_field_lost_to_a_syntax_error() {
        let lib = "export class D { x: number = 1; y: = 2; m(): number { return 1; } }\n";
        let main = "import { D } from './lib';\nconst d = new D();\n\
            console.log(d.x + d.y + d.m());\nconst { y } = d;\nconst s: string = 1;\n";
        assert_eq!(importer_errors(main, lib, 1), OWN);
    }

    #[test]
    fn check_says_nothing_about_an_interface_member_lost_to_a_syntax_error() {
        let lib = "export interface J { a: number; b: string; c number; d: boolean }\n";
        let main = "import { J } from './lib';\n\
            const j: J = { a: 1, b: \"x\", c: 2, d: true };\nconsole.log(j.c);\n\
            const s: string = 1;\n";
        assert_eq!(importer_errors(main, lib, 1), OWN);
    }

    #[test]
    fn check_says_nothing_about_a_subclass_method_lost_to_a_syntax_error() {
        let lib = "export class Base { k(): number { return 1; } }\n\
            export class Sub extends Base { n(: number { return 2; } o(): number { return 3; } }\n";
        let main = "import { Sub } from './lib';\nconst t = new Sub();\n\
            console.log(t.k() + t.n() + t.o());\nconst s: string = 1;\n";
        assert_eq!(importer_errors(main, lib, 1), OWN);
    }

    #[test]
    fn check_says_nothing_about_a_class_body_lost_to_a_syntax_error() {
        // The class keeps its name but loses its whole body; a subclass declared in the
        // importer doesn't have the members either. A member `lib` never mentions is still an
        // error.
        let lib = "export class W { a: number = 1 b(): number { return 2; } }\n";
        let main = "import { W } from './lib';\nclass X extends W {}\nconst w = new W();\n\
            console.log(w.a + w.b(), new X().b(), w.nope);\nconst s: string = 1;\n";
        let nope = "main.ts: no field `nope` on type `W`\n";
        assert_eq!(importer_errors(main, lib, 1), format!("{nope}{OWN}"));
    }

    #[test]
    fn check_says_nothing_about_a_class_lost_to_a_syntax_error() {
        let lib = "export class W implements { a: number = 1; b(): number { return 2; } }\n";
        let main = "import { W } from './lib';\nconst w = new W();\nconst s: string = 1;\n";
        assert_eq!(importer_errors(main, lib, 3), OWN);
    }

    #[test]
    fn builds_still_stop_at_syntax_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.ts"), BROKEN).unwrap();
        let main = "import { g } from './b';\nconst s: string = g();\n";
        std::fs::write(dir.path().join("a.ts"), main).unwrap();
        let opts = BuildOptions {
            input: dir.path().join("a.ts"),
            ..Default::default()
        };
        let mut sess = Session::new();
        let r = compile_to_vir(&mut sess, &opts);
        assert!(matches!(r, Err(BuildError::Diagnostics)), "{r:?}");
        let errors = sess.diagnostics.iter().filter(|d| d.is_error()).count();
        assert_eq!(errors, 1, "{}", sess.render_diagnostics());
    }
}
