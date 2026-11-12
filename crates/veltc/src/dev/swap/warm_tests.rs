//! The warm front end against the cold one (dev-warm-front-end.md step 3): a reload from the kept
//! check either refuses or gives exactly the program, and the diagnostics, of a cold build of the
//! edited sources. Hand-written edits for each way a reload once went wrong, and random body edits
//! of programs from the golden tests.

use std::path::Path;

use crate::dev::edits::{located, Snapshot};
use crate::driver::{self, BuildOptions, Session};

/// A cold build kept, and the reloads of edits from it, each compared with a cold build.
struct Differential {
    dir: tempfile::TempDir,
    opts: BuildOptions,
    kept: Option<velt_sema::KeptCheck>,
    base: Snapshot,
    cold_files: velt_common::SourceMap,
}

/// What one reload did.
#[derive(Debug)]
enum Reload {
    /// The warm program, which is the cold one.
    Warm,
    /// The cold front end, and why.
    Refused(String),
}

fn write(dir: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(path, text).expect("written");
    }
}

impl Differential {
    /// A cold build of `files` (relative path, text; the root `main.velt` first), kept; `None`
    /// when it fails.
    fn start(files: &[(&str, &str)]) -> Option<Differential> {
        let dir = tempfile::tempdir().expect("a temporary directory");
        write(dir.path(), files);
        let opts = BuildOptions {
            input: dir.path().join("main.velt"),
            ..BuildOptions::default()
        };
        // A check in which a generator ran is not kept: the next build uses its code from the
        // cache.
        let cold_build = || {
            let mut sess = Session::new();
            sess.keep_check = true;
            sess.counted_seed = Some(velt_vir::CountedSeed::default());
            let ok = driver::compile_for_jit(&mut sess, &opts).is_ok();
            (sess, ok)
        };
        let (mut sess, ok) = cold_build();
        if !ok {
            return None;
        }
        if sess.kept.is_none() {
            sess = cold_build().0;
        }
        let kept = sess.kept.take();
        assert!(kept.is_some(), "a dev build keeps its check");
        Some(Differential {
            base: Snapshot::of_build(&sess, &opts.input),
            cold_files: driver::copy_files(&sess.sm),
            dir,
            opts,
            kept,
        })
    }

    /// The sources as `files` now (the others as they were): the warm reload from the kept
    /// check, compared with a cold build. A warm program must be the cold one, with nothing to
    /// report; a reload may refuse, but never by an internal error.
    fn reload(&mut self, files: &[(&str, &str)]) -> Reload {
        write(self.dir.path(), files);
        let mut warm = Session::new();
        warm.kept = self.kept.take();
        warm.counted_seed = Some(velt_vir::CountedSeed::default());
        let (base, input) = (self.base.clone(), self.opts.input.clone());
        let edited = move |s: &Session| located(&base, &Snapshot::of_build(s, &input));
        let out = driver::compile_for_jit_warm(&mut warm, &self.opts, &self.cold_files, edited);
        let out = out.unwrap_or_else(|e| panic!("{e:?}"));
        let why = warm.warm_refused.take().unwrap_or_default();
        assert!(
            !why.contains("the warm front end failed"),
            "{files:?}: {why}"
        );
        self.kept = warm.kept.take();
        assert!(
            self.kept.is_some(),
            "the kept check stays for the next reload"
        );
        let Some(program) = out else {
            return Reload::Refused(why);
        };
        let mut cold = Session::new();
        cold.counted_seed = Some(velt_vir::CountedSeed::default());
        let built = driver::compile_for_jit(&mut cold, &self.opts);
        let diags = cold.render_diagnostics();
        let built = built.unwrap_or_else(|e| panic!("warm, but cold fails: {e:?}\n{diags}"));
        assert!(
            diags.trim().is_empty(),
            "{files:?}: warm reports nothing, cold: {diags}"
        );
        let (warm, cold) = (program.to_string(), built.to_string());
        if warm != cold {
            let first = (warm.lines().zip(cold.lines())).find(|(w, c)| w != c);
            panic!("{files:?}: the warm program is not the cold one: {first:?}");
        }
        Reload::Warm
    }
}

/// `before` built cold, then each edit of `main.velt` (a whole new text) reloaded from it.
fn reloads(before: &str, edits: &[String]) -> Vec<Reload> {
    let mut d = Differential::start(&[("main.velt", before)]).expect("the program builds");
    (edits.iter())
        .map(|e| d.reload(&[("main.velt", e.as_str())]))
        .collect()
}

/// `before` edited by replacing `from` with `to`, reloaded, then the edit undone.
fn edit(before: &str, from: &str, to: &str) -> Vec<Reload> {
    assert!(before.contains(from), "{from}");
    reloads(before, &[before.replacen(from, to, 1), before.to_string()])
}

/// The first reload of `out` refused, for a reason that says `why`.
fn refused(out: &[Reload], why: &str) {
    assert!(
        matches!(&out[0], Reload::Refused(r) if r.contains(why)),
        "{out:?}, not `{why}`"
    );
}

/// The edits of [`a_body_edit_reaching_std_code_first_refuses`]: each edited function is the
/// last definition of its file (`main` first), so the edit moves nothing, and the first in the
/// program to reach that std code.
const FIRST_STD_USES: &[(&str, &str, &str)] = &[
    (
        "function main() { console.log(count([\"a\", \"bb\"])); }
function count(words: string[]): number {
  let n = 0;
  for (const w of words) n += w.length;
  return n;
}
",
        "n += w.length",
        "n += Math.max(w.length, 1)",
    ),
    (
        "function main() { console.log(size()); }
function size(): number { return 1; }
",
        "return 1;",
        "const m = new Map<string, number>(); m.set(\"a\", 2); return m.size;",
    ),
    (
        "function main() { console.log(shout(\"a\")); }
function shout(s: string): string { return s + \"!\"; }
",
        "return s + \"!\";",
        "return s.toUpperCase() + \"!\";",
    ),
    (
        "function main() { console.log(f([3, 1, 2])); }
function f(xs: number[]): number { return xs.length; }
",
        "return xs.length;",
        "return xs.slice().sort((a, b) => a - b)[0];",
    ),
    (
        "async function main() { console.log(await later()); }
function later(): Promise<number> {
  let c = 0;
  return new Promise<number>((resolve) => { resolve(c); });
}
",
        "{ resolve(c); }",
        "{ setTimeout(() => { c += 1; resolve(c); }, 1); }",
    ),
    (
        "async function main() {
  const out: number[] = [];
  await Promise.all([1, 2].map(async (x) => { out.push(x); }));
  console.log(out.length);
}
",
        "{ out.push(x); }",
        "{ await null; out.push(x * 2); }",
    ),
];

/// A body that first reaches a std function: its code was never checked or made live, so the
/// reload refuses (it once failed in lowering, killing `velt dev`).
#[test]
fn a_body_edit_reaching_std_code_first_refuses() {
    for (src, from, to) in FIRST_STD_USES {
        refused(&edit(src, from, to), "std");
    }
}

/// A setter beside its getter, and an instance method beside a static one of the same name:
/// the edited one is checked again, not the first of that name.
#[test]
fn an_edited_setter_or_same_named_method_reloads_its_own_body() {
    // `main` first: the edited members are the last definitions, whose length may change.
    let setter = "function main() { const t = new T(); t.v = 5; console.log(t.v); }
class T {
  _v: number = 0;
  get v(): number { return this._v; }
  set v(x: number) { this._v = x; }
}
";
    let out = edit(setter, "this._v = x;", "this._v = x * 100;");
    assert!(matches!(out[0], Reload::Warm), "{out:?}");
    let getter = edit(setter, "return this._v;", "return this._v + 1;");
    // The getter is followed by the setter, which the longer edit moves.
    assert!(
        matches!(&getter[0], Reload::Refused(why) if why.contains("moves")),
        "{getter:?}"
    );
    let methods = "function main() { console.log(K.make(), new K().make()); }
class K {
  static make(): number { return 1; }
  make(): number { return 2; }
}
";
    let out = edit(methods, "return 2;", "return 200;");
    assert!(matches!(out[0], Reload::Warm), "{out:?}");
    let out = edit(methods, "return 1;", "return 7;");
    assert!(matches!(out[0], Reload::Warm), "{out:?}");
}

fn golden(name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/lang");
    std::fs::read_to_string(root.join(format!("{name}.velt"))).expect("a golden program")
}

/// Two functions edited in one reload: their first checks are undone together, so that neither
/// undoes what a third body's check added (once a closure lost a copy no edit touched).
#[test]
fn two_bodies_edited_at_once_leave_the_others_alone() {
    let src = golden("promise_resolve_stored");
    let both = (src.replacen(
        "setTimeout(() => done(), 0)",
        "setTimeout(() => done(), 1)",
        1,
    ))
    .replacen("\"finished\"", "\"finisheZ\"", 1);
    assert_ne!(both, src);
    let out = reloads(&src, &[both, src.clone()]);
    assert!(matches!(out[0], Reload::Warm), "{out:?}");
}

/// An edit that changes a body's length moves what follows it in the file, whose kept code
/// holds the old positions: refused. The last definition of a file moves nothing.
#[test]
fn a_length_changing_edit_refuses_unless_nothing_follows() {
    let src = "function a(): string {
  return \"x\";
}
function b(xs: number[]): number {
  if (xs.length > 10) throw new Error(\"big\");
  return xs[3] * 2;
}
function main() {
  console.log(a());
  try { console.log(b([1, 2, 3, 4])); } catch (e) { console.log(String(e)); }
}
";
    refused(
        &edit(
            src,
            "return \"x\";",
            "const pad = 1;\n  return \"x\" + pad;",
        ),
        "moves",
    );
    // Even on its line: the next function's column moves.
    let callback = "const handlers: Array<(n: number) => number> = [];
function register(): void { handlers.push((n) => n + 1); } function f(): number { return 1; }
function main() { register(); console.log(handlers[0](1), f()); }
";
    let out = edit(
        callback,
        "handlers.push((n) => n + 1);",
        "let k = 5; handlers.push((n) => n + k); k = 6;",
    );
    refused(&out, "moves");
    let last = "function main() { console.log(twice(2)); }
function twice(n: number): number { return n * 2; }
";
    let out = edit(last, "return n * 2;", "const k = 2;\n  return n * k + 0;");
    assert!(matches!(out[0], Reload::Warm), "{out:?}");
}

/// What the source scans find of the whole program follows the edit: a generator's `throw`
/// called for the first time compiles its `catch`.
#[test]
fn an_edit_changing_what_the_source_scans_find_refuses() {
    let src = "function main() { console.log(drive()); }
function* gen(): Generator<number> {
  try { yield 1; yield 2; } catch (e) { console.log(\"caught\"); }
}
function drive(): number {
  const g = gen();
  const r = g.next();
  return r.done ? 0 : r.value;
}
";
    let out = edit(
        src,
        "const r = g.next();",
        "g.next(); const r = g.throw(new Error(\"x\"));",
    );
    refused(&out, "scans");
}

/// The edited body's warnings are the reload's: it refuses, and the cold front end reports them.
#[test]
fn an_edit_with_a_warning_refuses() {
    let src = "function main() { console.log(f()); }
function f(x?: number): number {
  return x ?? 0;
}
";
    let out = edit(
        src,
        "return x ?? 0;",
        "if (x === null) return 1; return x ?? 0;",
    );
    refused(&out, "always false");
}

/// What a function throws is read by its callers' checks (a `catch` nothing reaches is left
/// out): an edit that changes it refuses. The edited function is the last definition, so that
/// nothing moves.
#[test]
fn an_edit_changing_what_a_function_throws_refuses() {
    let src = "function main() {
  try { console.log(risky(1)); } catch (e) {
    if (e instanceof MyErr) console.log(\"my\", e.code); else console.log(\"other\");
  }
}
class MyErr extends Error { code: number = 3; }
function risky(n: number): number { if (n > 0) throw new MyErr(\"boom\"); return n; }
";
    let from = "if (n > 0) throw new MyErr(\"boom\"); return n;";
    let to = format!("{:<1$}", "return n;", from.len());
    refused(&edit(src, from, &to), "throws");
    let to = format!(
        "{:<1$}",
        "if (n > 0) throw new Error(\"boom\"); return n;",
        from.len()
    );
    refused(&edit(src, from, &to), "throws");
}

/// A small deterministic generator (xorshift), for the random edits.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

/// The byte ranges of the bodies of `src`'s functions and methods (the script's statements and
/// nested functions aside).
fn bodies(src: &str) -> Vec<(usize, usize)> {
    use velt_syntax::ast::ItemKind;
    let (module, _) = velt_syntax::parse_file(velt_common::FileId(0), src);
    let mut out = vec![];
    for item in &module.items {
        match &item.kind {
            ItemKind::Function(f) if !velt_syntax::is_script_main(&f.sig.name) => {
                out.push(f.body.span)
            }
            ItemKind::Class(t) | ItemKind::Struct(t) => {
                out.extend(t.constructor.as_ref().map(|c| c.body.span));
                out.extend(t.methods.iter().map(|m| m.decl.body.span));
            }
            _ => {}
        }
    }
    (out.into_iter())
        .map(|s| (s.lo as usize, s.hi as usize))
        .filter(|&(lo, hi)| lo < hi && hi <= src.len())
        .collect()
}

/// One random edit inside a body of `src` (the last one, or any): a number literal changed (to one of the same length,
/// or another), or a string literal changed, or nothing when the body it picked has neither.
fn mutate(src: &str, rng: &mut Rng) -> Option<String> {
    let mut bodies = bodies(src);
    bodies.sort_unstable();
    // Half the edits are in the file's last function, which may change length and still reload
    // warm (an edit elsewhere that does refuses: it moves what follows).
    let pick = match rng.below(2) {
        0 => bodies.len().checked_sub(1)?,
        _ => rng.below(bodies.len()),
    };
    let &(lo, hi) = bodies.get(pick)?;
    let text = &src[lo..hi];
    let b = text.as_bytes();
    let word = |i: usize| b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.';
    let mut numbers = vec![];
    let mut strings = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !word(i - 1)) {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i == b.len() || !word(i) {
                numbers.push((start, i));
            }
            continue;
        }
        if b[i] == b'"' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != b'"' && b[i] != b'\\' && b[i] != b'\n' {
                i += 1;
            }
            if i < b.len() && b[i] == b'"' && i - start > 1 {
                strings.push((start + 1, i));
            }
        }
        i += 1;
    }
    let (s, e, new) = match rng.below(3) {
        0 | 1 if !numbers.is_empty() => {
            let (s, e) = numbers[rng.below(numbers.len())];
            let n: u64 = text[s..e].parse().ok()?;
            let new = match rng.below(3) {
                0 => (n % 9 + 1 + n / 10 * 10).to_string(),
                1 => (n + 1).to_string(),
                _ => (n * 10 + 7).to_string(),
            };
            (s, e, new)
        }
        _ if !strings.is_empty() => {
            let (s, e) = strings[rng.below(strings.len())];
            let new = match rng.below(2) {
                0 => format!("{}Z", &text[s..e - 1]),
                _ => format!("{}!", &text[s..e]),
            };
            (s, e, new)
        }
        _ => return None,
    };
    Some(format!(
        "{}{}{}{}",
        &src[..lo],
        &text[..s],
        new,
        &src[lo + e..]
    ))
}

/// Random body edits of programs from the golden tests, each reloaded from the first version's
/// kept check and compared with a cold build of it (`Differential::reload`), then undone.
#[test]
fn random_body_edits_reload_warm_to_the_cold_program() {
    let programs = [
        "promise_resolve_stored",
        "opt_number_arrays_doubles",
        "switch_nullable_case",
        "generator_methods",
        "instanceof_downcast",
        "narrow_typeof_printable",
        "callback_union_two_error_slots",
        "shared_fn_call",
        "generator_default_value",
    ];
    let (mut warm, mut refused) = (0, 0);
    for (k, name) in programs.iter().enumerate() {
        let src = golden(name);
        let Some(mut d) = Differential::start(&[("main.velt", &src)]) else {
            continue;
        };
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ ((k as u64 + 1) * 0x1234_5678));
        let mut now = src.clone();
        for _ in 0..4 {
            let Some(next) = mutate(&now, &mut rng) else {
                continue;
            };
            now = next;
            match d.reload(&[("main.velt", &now)]) {
                Reload::Warm => warm += 1,
                Reload::Refused(_) => refused += 1,
            }
        }
        assert!(
            matches!(d.reload(&[("main.velt", &src)]), Reload::Warm),
            "{name}: undone"
        );
    }
    // The comparison means something only if some reloads were warm.
    assert!(warm > 0, "{warm} warm, {refused} refused");
}

/// A generator with an `any` default used as a value goes through a wrapper made by the reading
/// body's check (`gen_value.rs`): an edit of that body makes the wrapper again, with the copy of
/// its argument's coercion (once the kept wrapper was reused without it).
#[test]
fn an_edit_of_a_body_reading_a_generator_value_remakes_its_wrapper() {
    let src = "function* gen(x: any = \"g\"): Generator<string> {
  yield `${x}`;
}
function main(): void {
  const gv = gen;
  console.log(gv(undefined).next().value, \"a\");
}
";
    let out = edit(src, "\"a\"", "\"b\"");
    assert!(matches!(out[0], Reload::Warm), "{out:?}");
}

/// `VELT_DEV_WARM=off` (`Carried::warm`) keeps every save on the cold front end, also under `-v`,
/// which keeps a check to measure it.
#[test]
fn warm_off_reloads_cold_though_a_check_is_kept() {
    let src = "function main() { console.log(twice(2)); }
function twice(n: number): number { return n * 2; }
";
    for warm in [true, false] {
        let dir = tempfile::tempdir().expect("a temporary directory");
        write(dir.path(), &[("main.velt", src)]);
        let opts = BuildOptions {
            input: dir.path().join("main.velt"),
            ..BuildOptions::default()
        };
        let mut sess = Session::new();
        sess.keep_check = true;
        sess.counted_seed = Some(velt_vir::CountedSeed::default());
        let first = driver::compile_for_jit(&mut sess, &opts).unwrap_or_else(|e| panic!("{e:?}"));
        let mut session =
            velt_codegen_cl::DevSession::new(&velt_rt_host::abi_symbols::symbol_table());
        session.load(&first).expect("load");
        let own = Snapshot::of_build(&sess, &opts.input);
        let mut carried = super::Carried {
            seed: sess.counted_seed.take(),
            own: Some(own.clone()),
            interfaces: None,
            kept: sess.kept.take(),
            kept_own: Some(own),
            kept_files: Some(driver::copy_files(&sess.sm)),
            warm,
            warm_reloads: 0,
        };
        assert!(carried.kept.is_some(), "a dev build keeps its check");
        write(dir.path(), &[("main.velt", &src.replace("n * 2", "n * 3"))]);
        let flags = super::Report {
            verbose: true,
            details: false,
        };
        let reloaded = super::reload_with(&mut session, &opts, flags, &mut carried, |_| Ok(()));
        assert!(
            !matches!(
                reloaded,
                velt_rt_host::dev::handover::reload::Reloaded::Failed { .. }
            ),
            "{reloaded:?}"
        );
        assert_eq!(carried.warm_reloads, usize::from(warm), "warm: {warm}");
        assert!(carried.kept.is_some(), "a check is kept for the next save");
    }
}
