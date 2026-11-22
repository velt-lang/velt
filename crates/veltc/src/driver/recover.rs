//! `velt check` past syntax errors (#1155): the modules without any are type-checked too.

use std::time::Instant;

use super::{load_modules, sema_stage, BuildError, BuildOptions, CheckScope, Session, StdLoad};

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
    check_recovering_loaded(sess, opts, scope).and_then(|(_, checked)| checked)
}

/// [`check_recovering`] that also returns the loaded modules, for a lint of them (`velt check
/// --perf`): `Err` when loading failed, so nothing was checked.
pub(super) fn check_recovering_loaded(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<(velt_loader::Loaded, Result<(), BuildError>), BuildError> {
    // A std module the check demands that can't be served in the middle of it is loaded up front
    // for a check again, as `sema_stage` does.
    let mut scope = std::borrow::Cow::Borrowed(scope);
    loop {
        let diags_before = sess.diagnostics.len();
        let files_before = sess.sm.files().count();
        let (loaded, checked, refused) = check_recovering_once(sess, opts, &scope)?;
        let fresh: Vec<String> = (refused.into_iter())
            .filter(|p| !scope.preload.contains(p))
            .collect();
        if fresh.is_empty() {
            return Ok((loaded, checked));
        }
        sess.diagnostics.truncate(diags_before);
        let mut sm = velt_common::SourceMap::new();
        for (_, f) in sess.sm.files().take(files_before) {
            sm.add(f.path.clone(), f.src.clone());
        }
        sess.sm = sm;
        scope.to_mut().preload.extend(fresh);
    }
}

/// What [`check_recovering_once`] gives: what it loaded, the check's result, and the std modules
/// it demanded that could not be served in place.
type Recovered = (velt_loader::Loaded, Result<(), BuildError>, Vec<String>);

/// One load and check of [`check_recovering_loaded`].
fn check_recovering_once(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<Recovered, BuildError> {
    let mut loaded = load_modules(sess, opts, scope)?;
    if !sess.has_errors() {
        // `sema_stage` checks again itself.
        let checked = sema_stage(sess, opts, scope, &mut loaded).map(drop);
        return Ok((loaded, checked, vec![]));
    }
    let t = Instant::now();
    let broken: std::collections::HashSet<velt_common::FileId> = sess
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .filter_map(primary_span)
        .map(|span| span.file)
        .collect();
    // Owned: the std modules the check demands are added to the source map meanwhile.
    let sources: Vec<(usize, String)> = (loaded.modules.iter().enumerate())
        .filter(|(_, m)| broken.contains(&m.file))
        .map(|(i, m)| (i, sess.sm.get(m.file).src.clone()))
        .collect();
    let recovered: Vec<(usize, &str)> = sources.iter().map(|(i, s)| (*i, s.as_str())).collect();
    let sema_opts = velt_sema::CheckOptions {
        require_main: scope.require_main,
        eager_std: scope.eager_std,
        units: scope.units,
        std_key: None,
        native_executor: super::native_executor(opts),
        wasm32: super::wasm32(opts),
    };
    // The std modules the check demands are served as for a program that loaded cleanly
    // (`sema_stage`): the load left them to the check (#3150).
    let finder = super::prebuilt_finder(sess, opts, scope);
    let more = (!scope.eager_std).then(|| {
        StdLoad::new(
            (&mut loaded.state, &loaded.modules),
            sess.generator_build,
            (opts, scope),
            finder.as_ref(),
        )
    });
    let has_more = more.is_some();
    let linked = |hook: usize| finder.as_ref().is_some_and(|f| f.linked(hook));
    let prebuilt = finder.as_ref().map(|_| &linked as &velt_sema::PrebuiltFn);
    let kept = velt_sema::StdLoads::default();
    let shared = std::sync::Mutex::new((&mut *sess, more));
    let serve = |demanded: &[String]| {
        let mut guard = shared.lock().ok()?;
        let (sess, more) = &mut *guard;
        Some(more.as_mut()?.serve(sess, demanded))
    };
    let loads = has_more.then_some(velt_sema::Loads {
        run: &serve,
        kept: &kept,
        prebuilt,
    });
    // The recovered trees can hold shapes no clean parse produces: a type checker failure on
    // them costs these findings only, never the syntax errors already reported.
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        velt_sema::check_recovered(&loaded.modules, loaded.root, sema_opts, &recovered, loads)
    }));
    let (sess, more) = match shared.into_inner() {
        Ok(inner) => inner,
        Err(poisoned) => poisoned.into_inner(),
    };
    let refused = more.map_or(vec![], |m| m.refused);
    if !refused.is_empty() {
        return Ok((loaded, Err(BuildError::Diagnostics), refused));
    }
    if let Ok(diags) = checked {
        let found = (diags.into_iter())
            .filter(|d| primary_span(d).is_none_or(|span| !broken.contains(&span.file)));
        sess.diagnostics.extend(found);
    }
    sess.record("sema", t);
    Ok((loaded, Err(BuildError::Diagnostics), vec![]))
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
        errors_of(&check_session(files))
    }

    /// The session of `velt check` of the files `files`, which must fail.
    fn check_session(files: &[(&str, &str)]) -> Session {
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
        sess
    }

    /// Each error of `sess` as `file: message`, one per line.
    fn errors_of(sess: &Session) -> String {
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

    /// The number hooks' modules (std/lang/hooks/number_digits, number_parse) `sess` loaded.
    fn number_hook_modules(sess: &Session) -> Vec<String> {
        let paths = sess
            .sm
            .files()
            .map(|(_, f)| f.path.to_string_lossy().replace('\\', "/"));
        let hooks = [
            "lang/hooks/number_digits.velt",
            "lang/hooks/number_parse.velt",
        ];
        let mut held: Vec<String> = paths
            .filter_map(|p| {
                hooks
                    .iter()
                    .find(|h| p.ends_with(*h))
                    .map(|h| h.to_string())
            })
            .collect();
        held.sort();
        held.dedup();
        held
    }

    /// Past a syntax error, the check serves the std modules it demands as a clean check does
    /// (#3150): `toFixed` past 64 bits, `toPrecision`, `toExponential`, `parseInt`'s and
    /// `Number`'s long paths and `toFixed` through `any` call their hooks by role, and reported
    /// them as not loaded.
    #[test]
    fn check_past_a_syntax_error_loads_the_hooks_it_reaches() {
        let main = "import { g } from './b';\n\
            console.log(g(), (1.5).toFixed(30), (1.5).toPrecision(25), (1.5).toExponential(3));\n\
            console.log(parseInt(\"123456789012345678901234\"));\n\
            console.log(Number(\"1.000000000000000000000001\"));\n\
            const a: any = 2.5;\nconsole.log(a.toFixed(30));\n\
            const s: string = 1;\n";
        let sess = check_session(&[("main.ts", main), ("b.ts", BROKEN)]);
        assert_eq!(
            errors_of(&sess),
            "b.ts: expected identifier, found `{`\nmain.ts: mismatched types\n"
        );
        let held = number_hook_modules(&sess);
        assert_eq!(
            held,
            [
                "lang/hooks/number_digits.velt",
                "lang/hooks/number_parse.velt"
            ]
        );
    }

    /// A program that calls none of them loads neither module, past a syntax error or not.
    #[test]
    fn check_loads_no_number_hooks_a_program_does_not_reach() {
        let main = "import { g } from './b';\nconsole.log(g(), `${1.5}`);\nconst s: string = 1;\n";
        let past = check_session(&[("main.ts", main), ("b.ts", BROKEN)]);
        assert_eq!(number_hook_modules(&past), Vec::<String>::new());
        let clean = "export function g(): number { return 1; }\n";
        let sess = check_session(&[("main.ts", main), ("b.ts", clean)]);
        assert_eq!(errors_of(&sess), "main.ts: mismatched types\n");
        assert_eq!(number_hook_modules(&sess), Vec::<String>::new());
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
