//! Differential execution: VIR programs are run by `velt_opt`'s reference interpreter and,
//! compiled through the LLVM backend (`emit_ir` + `clang -O3`), natively. Everything is linked
//! into one executable with a generated Rust harness that defines the externs (recording their
//! calls like `RecordingHost`) and calls each run's entry, so there is a single `rustc` link.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use velt_opt::interp::{Arg, Interp, RecordingHost};
use velt_vir::vir::{ExternFn, Function, Linkage, Program, Ty};

/// One program and the calls to make: (entry symbol, raw-bit args).
pub struct Subject {
    pub name: String,
    pub program: Program,
    pub runs: Vec<(String, Vec<u64>)>,
}

/// A run whose interpreted outcome is known.
struct Expected {
    id: String,
    entry: String,
    params: Vec<Ty>,
    ret: Ty,
    args: Vec<u64>,
    text: String,
}

/// Tools needed, or `None` (with a note) when unavailable.
pub fn tools() -> Option<(PathBuf, String)> {
    let Some(clang) = velt_codegen_llvm::find_clang() else {
        eprintln!("note: clang not available; skipping native LLVM tests");
        return None;
    };
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let ok = command(&rustc)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("note: rustc not available; skipping native LLVM tests");
        return None;
    }
    Some((clang, rustc))
}

/// Run every subject both ways and panic with all mismatches.
pub fn check(subjects: Vec<Subject>, tag: &str) {
    let Some((clang, rustc)) = tools() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("velt_llvm_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let mut expected = vec![];
    let mut externs = BTreeMap::new();
    let mut lls = vec![];
    for (i, s) in subjects.iter().enumerate() {
        let prefix = format!("c{i}_");
        let program = prepare(&s.program, &s.runs);
        for e in &program.externs {
            externs.insert(e.symbol.clone(), e.clone());
        }
        expected.extend(interpret(&program, s, &prefix));
        let ir = velt_codegen_llvm::emit_ir(&program, "")
            .unwrap_or_else(|e| panic!("{}: emit_ir failed: {e}\n{program}", s.name));
        let ll = dir.join(format!("c{i}.ll"));
        std::fs::write(&ll, rename_exports(&ir, &program, &prefix)).expect("write .ll");
        lls.push(ll);
    }
    let objects = compile_all(&clang, &lls);
    let harness = dir.join("harness.rs");
    std::fs::write(&harness, harness_source(&externs, &expected)).expect("write harness");
    let output = link_and_run(&rustc, &dir, &harness, &objects);
    let _ = std::fs::remove_dir_all(&dir);
    compare(&expected, &output);
}

/// Run entries become exported; programs without the `velt_main` the verifier requires get a
/// stub one.
fn prepare(program: &Program, runs: &[(String, Vec<u64>)]) -> Program {
    let mut p = program.clone();
    for f in &mut p.funcs {
        if runs.iter().any(|(entry, _)| *entry == f.symbol) {
            f.linkage = Linkage::Export;
        }
    }
    if !p.funcs.iter().any(|f| f.symbol == "velt_main") {
        p.funcs.push(main_stub());
    }
    p
}

fn main_stub() -> Function {
    use velt_vir::vir::{BasicBlock, Const, LocalDecl, Operand, Terminator};
    Function {
        locals: vec![LocalDecl::new(Ty::I32, None)],
        blocks: vec![BasicBlock {
            stmts: vec![],
            term: Terminator::Return(Operand::Const(Const::Int(0), Ty::I32)),
        }],
        ..Function::new("velt_main".into(), vec![], Ty::I32, Linkage::Export)
    }
}

/// Exported symbols get a per-program prefix so all programs link into one executable.
fn rename_exports(ir: &str, program: &Program, prefix: &str) -> String {
    let mut ir = ir.to_string();
    for f in program
        .funcs
        .iter()
        .filter(|f| f.linkage == Linkage::Export)
    {
        let from = format!("@\"{}\"", f.symbol);
        ir = ir.replace(&from, &format!("@\"{prefix}{}\"", f.symbol));
    }
    ir
}

/// Canonical text of a raw-bit scalar (NaN payloads are not portable across constant folding).
fn show(ty: Ty, bits: u64) -> String {
    match ty {
        Ty::F64 if f64::from_bits(bits).is_nan() => "nan".into(),
        Ty::F32 if f32::from_bits(bits as u32).is_nan() => "nan".into(),
        Ty::Ptr => "p".into(),
        _ => bits.to_string(),
    }
}

fn interpret(program: &Program, s: &Subject, prefix: &str) -> Vec<Expected> {
    let mut out = vec![];
    for (k, (entry, args)) in s.runs.iter().enumerate() {
        let mut interp = Interp::new(program, RecordingHost::default());
        let Ok(result) = interp.call_symbol(entry, args) else {
            continue; // Panicking runs cannot continue natively; the interpreter covers them.
        };
        let f = program
            .funcs
            .iter()
            .find(|f| f.symbol == *entry)
            .expect("entry exists");
        let mut text = String::new();
        for call in &interp.host.calls {
            let ext = program.externs.iter().find(|e| e.symbol == call.symbol);
            let _ = write!(text, "call {}", call.symbol);
            for (i, a) in call.args.iter().enumerate() {
                let ty = ext.map_or(Ty::I64, |e| e.params[i]);
                match a {
                    Arg::Ptr => text.push_str(" p"),
                    Arg::Bits(b) => {
                        let _ = write!(text, " {}", show(ty, *b));
                    }
                }
            }
            text.push('\n');
        }
        if f.ret != Ty::Unit {
            let _ = writeln!(text, "ret {}", show(f.ret, result));
        }
        out.push(Expected {
            id: format!("{} {entry}#{k}", s.name),
            entry: format!("{prefix}{entry}"),
            params: f.params.clone(),
            ret: f.ret,
            args: args.clone(),
            text,
        });
    }
    out
}

/// Compile all `.ll` files with `clang -O3`, several at a time.
pub fn compile_all(clang: &Path, lls: &[PathBuf]) -> Vec<PathBuf> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = lls.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for part in lls.chunks(chunk) {
            scope.spawn(move || {
                for ll in part {
                    let out = command(clang)
                        .args(["-O3", "-c", "-x", "ir", "-Wno-override-module"])
                        .arg(ll)
                        .arg("-o")
                        .arg(ll.with_extension("o"))
                        .output()
                        .expect("run clang");
                    assert!(
                        out.status.success(),
                        "clang rejected {}:\n{}",
                        ll.display(),
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
            });
        }
    });
    lls.iter().map(|ll| ll.with_extension("o")).collect()
}

fn rust_type(ty: Ty) -> &'static str {
    match ty {
        Ty::I8 => "i8",
        Ty::I16 => "i16",
        Ty::I32 => "i32",
        Ty::I64 => "i64",
        Ty::U8 | Ty::Bool => "u8",
        Ty::U16 => "u16",
        Ty::U32 => "u32",
        Ty::U64 => "u64",
        Ty::F32 => "f32",
        Ty::F64 => "f64",
        Ty::Ptr => "*const u8",
        Ty::Unit => "()",
        Ty::Agg(_) => panic!("aggregate in a signature"),
    }
}

/// Rust expression turning value `v` of `ty` into zero-extended raw bits (`u64`).
fn to_bits(ty: Ty, v: &str) -> String {
    match ty {
        Ty::I8 => format!("{v} as u8 as u64"),
        Ty::I16 => format!("{v} as u16 as u64"),
        Ty::I32 => format!("{v} as u32 as u64"),
        Ty::F32 => format!("{v}.to_bits() as u64"),
        Ty::F64 => format!("{v}.to_bits()"),
        Ty::Ptr => format!("{v} as usize as u64"),
        _ => format!("{v} as u64"),
    }
}

/// Rust expression turning raw bits `b` into a value of `ty`.
fn from_bits(ty: Ty, b: u64) -> String {
    match ty {
        Ty::F32 => format!("f32::from_bits({}u32)", b as u32),
        Ty::F64 => format!("f64::from_bits({b}u64)"),
        Ty::Ptr => format!("{b}usize as *const u8"),
        t => format!("{b}u64 as {}", rust_type(t)),
    }
}

fn harness_source(externs: &BTreeMap<String, ExternFn>, runs: &[Expected]) -> String {
    let mut s = String::from(HARNESS_PRELUDE);
    for (name, e) in externs {
        let params: Vec<String> = (0..e.params.len())
            .map(|i| format!("a{i}: {}", rust_type(e.params[i])))
            .collect();
        let ret = if e.noreturn {
            "!".to_string()
        } else {
            rust_type(e.ret).to_string()
        };
        let _ = writeln!(
            s,
            "#[no_mangle] pub extern \"C\" fn {name}({}) -> {ret} {{",
            params.join(", ")
        );
        let _ = write!(s, "    let mut line = String::from(\"call {name}\");");
        for (i, &ty) in e.params.iter().enumerate() {
            let _ = write!(
                s,
                " line.push_str(&format!(\" {{}}\", show({}, {})));",
                ty_code(ty),
                to_bits(ty, &format!("a{i}"))
            );
        }
        s.push_str(" println!(\"{line}\");");
        if e.noreturn {
            s.push_str(" std::process::exit(99)");
        } else if e.ret != Ty::Unit {
            let _ = write!(s, " {}", from_bits(e.ret, 0));
        }
        s.push_str("\n}\n");
    }
    s.push_str("extern \"C\" {\n");
    let mut declared = std::collections::HashSet::new();
    for r in runs.iter().filter(|r| declared.insert(r.entry.clone())) {
        let params: Vec<String> = r
            .params
            .iter()
            .map(|&t| format!("_: {}", rust_type(t)))
            .collect();
        let _ = writeln!(
            s,
            "    fn {}({}) -> {};",
            r.entry,
            params.join(", "),
            rust_type(r.ret)
        );
    }
    s.push_str("}\nfn main() {\n");
    for r in runs {
        let args: Vec<String> = r
            .params
            .iter()
            .zip(&r.args)
            .map(|(&t, &b)| from_bits(t, b))
            .collect();
        let _ = writeln!(s, "    println!(\"== {}\");", r.id);
        let _ = writeln!(
            s,
            "    let r = unsafe {{ {}({}) }};",
            r.entry,
            args.join(", ")
        );
        if r.ret != Ty::Unit {
            let _ = writeln!(
                s,
                "    println!(\"ret {{}}\", show({}, {}));",
                ty_code(r.ret),
                to_bits(r.ret, "r")
            );
        } else {
            s.push_str("    let () = r;\n");
        }
    }
    s.push_str("}\n");
    s
}

/// The harness's own `show` takes a type tag: 0 = int, 1 = f32, 2 = f64, 3 = pointer.
fn ty_code(ty: Ty) -> u8 {
    match ty {
        Ty::F32 => 1,
        Ty::F64 => 2,
        Ty::Ptr => 3,
        _ => 0,
    }
}

const HARNESS_PRELUDE: &str = r#"#![allow(unused_parens, clippy::all)]
fn show(tag: u8, bits: u64) -> String {
    match tag {
        1 if f32::from_bits(bits as u32).is_nan() => "nan".into(),
        2 if f64::from_bits(bits).is_nan() => "nan".into(),
        3 => "p".into(),
        _ => bits.to_string(),
    }
}
"#;

pub fn link_and_run(rustc: &str, dir: &Path, harness: &Path, objects: &[PathBuf]) -> String {
    let exe = dir.join(format!("harness{}", std::env::consts::EXE_SUFFIX));
    // Object paths go through an argument file: hundreds of them exceed Windows' command line.
    let mut args = String::new();
    for o in objects {
        let _ = writeln!(args, "-Clink-arg={}", o.display());
    }
    if cfg!(target_os = "linux") {
        // Link args come after rustc's libraries, so libm must follow the objects for `fmod`
        // (glibc on aarch64 has it only in libm; static musl has it in libc, already scanned).
        args.push_str("-Clink-arg=-lm\n");
        if cfg!(target_env = "musl") {
            args.push_str("-Clink-arg=-lc\n");
        }
    }
    let argfile = dir.join("link.args");
    std::fs::write(&argfile, args).expect("write argfile");
    let out = command(rustc)
        .args(["--edition", "2021", "-O", "--crate-name", "harness"])
        .arg(harness)
        .arg("-o")
        .arg(&exe)
        .arg(format!("@{}", argfile.display()))
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "linking the harness failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = command(&exe).output().expect("run harness");
    assert!(
        run.status.success(),
        "harness crashed ({:?}):\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout)
    );
    String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n")
}

fn compare(expected: &[Expected], output: &str) {
    let mut got: BTreeMap<&str, String> = BTreeMap::new();
    let mut current = None;
    for line in output.lines() {
        if let Some(id) = line.strip_prefix("== ") {
            current = Some(id);
            got.insert(id, String::new());
        } else if let Some(id) = current {
            let text = got.get_mut(id).expect("section");
            text.push_str(line);
            text.push('\n');
        }
    }
    let mut failures = vec![];
    for e in expected {
        let actual = got.get(e.id.as_str()).map_or("<missing>", |s| s.as_str());
        if actual != e.text {
            failures.push(format!("{}: interpreter\n{}native\n{actual}", e.id, e.text));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} runs differ:\n{}",
        failures.len(),
        expected.len(),
        failures.join("\n")
    );
    eprintln!("{} native runs match the interpreter", expected.len());
}

#[path = "../../../../tests/common/command.rs"]
mod command;
use command::command;
