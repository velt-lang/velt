//! std's start-up check of the CPU (rt_abi.md "CPU check") runs before anything knows what the
//! CPU has, so it must use no instruction beyond the architecture's baseline, whatever `--cpu`
//! says: on a CPU without the build's features it must print its message, not die of an illegal
//! instruction in its own code (#3293 review). `velt_vir::start_up` marks the check and what it
//! reaches (`--emit vir` marks them `baseline`), and both backends compile those functions for the
//! baseline. This disassembles them in objects for x86-64-v4 and v3, on any host with `objdump` or
//! `llvm-objdump` (no CPU or emulator needed), and finds no VEX or EVEX encoding, no wide or mask
//! register, and nothing from SSE3 up or BMI. Without either, it checks the LLVM IR's attributes.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

mod no_window;
mod test_dir;

const VELT: &str = env!("CARGO_BIN_EXE_velt");
const PROGRAM: &str = "function main() {\n  console.log(\"started\");\n}\n";

/// The functions `--emit vir` marks `baseline` (`vir::Function::baseline`), by symbol.
fn baseline_symbols(vir: &str) -> HashSet<String> {
    (vir.lines())
        .filter(|l| l.starts_with("fn#"))
        .filter(|l| {
            l.split(" -> ")
                .nth(1)
                .is_some_and(|r| r.split_whitespace().any(|w| w == "baseline"))
        })
        .filter_map(|l| {
            l.split_whitespace()
                .nth(2)?
                .split('(')
                .next()
                .map(str::to_string)
        })
        .collect()
}

/// Whether `instruction` (objdump's AT&T text, mnemonic first) needs more than x86-64's baseline.
fn above_baseline(instruction: &str) -> bool {
    const PLAIN: &[&str] = &[
        "popcnt",
        "lzcnt",
        "tzcnt",
        "movbe",
        "crc32",
        "ptest",
        "pmuldq",
        "pmulld",
        "pcmpeqq",
        "pcmpgtq",
        "packusdw",
        "pshufb",
        "palignr",
        "pmaddubsw",
        "pmulhrsw",
        "shrx",
        "shlx",
        "sarx",
        "rorx",
        "andn",
        "bzhi",
        "pdep",
        "pext",
        "mulx",
        "blsi",
        "blsr",
        "blsmsk",
        "cmpxchg16b",
        "lddqu",
        "movddup",
        "movshdup",
        "movsldup",
        "insertps",
        "extractps",
        "mpsadbw",
        "phminposuw",
        "pclmulqdq",
    ];
    const PREFIXES: &[&str] = &[
        "v", "pinsr", "round", "blend", "pblend", "pmovzx", "pmovsx", "pmins", "pminu", "pmaxs",
        "pmaxu", "pabs", "phadd", "phsub", "psign", "hadd", "hsub", "addsub", "dpp", "aes", "sha",
        "kmov", "kor", "kand",
    ];
    let mnemonic = instruction.split_whitespace().next().unwrap_or_default();
    // `pextrw` is SSE2; `pextrb`/`d`/`q` are SSE4.1.
    let pextr = mnemonic.starts_with("pextr") && mnemonic != "pextrw";
    PLAIN.contains(&mnemonic)
        || pextr
        || PREFIXES.iter().any(|p| mnemonic.starts_with(p))
        || instruction.contains("%ymm")
        || instruction.contains("%zmm")
        || (0..8).any(|k| {
            let reg = format!("%k{k}");
            instruction
                .split(|c: char| c == ',' || c.is_whitespace())
                .any(|w| w == reg)
        })
}

/// How many of `start_up`'s functions `object` has, and their instructions above the baseline, by
/// objdump; `None` where no objdump runs here.
fn disassembled(object: &Path, start_up: &HashSet<String>) -> Option<(usize, Vec<String>)> {
    let out = ["objdump", "llvm-objdump"].iter().find_map(|tool| {
        let o = Command::new(tool)
            .args(["-d", "--no-show-raw-insn"])
            .arg(object)
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    })?;
    let (mut functions, mut bad, mut current) = (0, vec![], None::<String>);
    for line in out.lines() {
        if let Some(name) = line.strip_suffix(">:").and_then(|l| l.split_once(" <")) {
            current = start_up.contains(name.1).then(|| name.1.to_string());
            functions += usize::from(current.is_some());
            continue;
        }
        let (Some(f), Some(text)) = (&current, line.split('\t').next_back()) else {
            continue;
        };
        if line.contains('\t') && above_baseline(text.trim()) {
            bad.push(format!("{f}: {}", text.trim()));
        }
    }
    Some((functions, bad))
}

#[test]
fn the_start_up_check_uses_only_the_baseline_cpu() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    std::fs::write(root.join("main.velt"), PROGRAM).unwrap();
    let mut builds = vec![
        ("debug v4", vec!["--cpu", "x86-64-v4"]),
        ("debug v3", vec!["--cpu", "x86-64-v3"]),
    ];
    if velt_codegen_llvm::available() {
        builds.push(("release v4", vec!["--release", "--cpu", "x86-64-v4"]));
        builds.push(("release v2", vec!["--release"]));
    }
    let build = |emit: &str, out: Option<&Path>, flags: &[&str]| {
        let mut args = vec![
            "build",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--emit",
            emit,
        ];
        args.extend(flags);
        if let Some(out) = out {
            args.extend(["-o", out.to_str().unwrap()]);
        }
        args.push("main.velt");
        let o = no_window::command(VELT)
            .args(&args)
            .current_dir(root)
            .env_remove("VELT_CPU")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    for (i, (what, flags)) in builds.iter().enumerate() {
        let start_up = baseline_symbols(&build("vir", None, flags));
        assert!(start_up.len() > 3, "{what}: marked {start_up:?}");
        let object = root.join(format!("start{i}.o"));
        build("obj", Some(&object), flags);
        match disassembled(&object, &start_up) {
            Some((functions, bad)) => {
                assert!(functions > 0, "{what}: no start-up function in the object");
                assert!(
                    bad.is_empty(),
                    "{what}: above the baseline:\n{}",
                    bad.join("\n")
                );
            }
            // No disassembler here: what LLVM is told instead (Cranelift's code goes unchecked).
            None if velt_codegen_llvm::available() => {
                let ir = build("llvm", None, flags);
                for line in ir.lines().filter(|l| l.starts_with("define ")) {
                    let symbol = line.split("@\"").nth(1).and_then(|r| r.split('"').next());
                    if symbol.is_some_and(|s| start_up.contains(s)) {
                        assert!(line.contains("\"target-cpu\"=\"x86-64\""), "{what}: {line}");
                    }
                }
            }
            None => eprintln!("{what}: no objdump and no LLVM: the start-up code goes unchecked"),
        }
    }
}

#[test]
fn the_baseline_test_knows_the_instructions_above_it() {
    for above in [
        "vpor   %xmm1,%xmm0,%xmm0",
        "shlx   %rsi,%rax,%rax",
        "andn   %rax,%rdx,%rax",
        "pinsrd $0x1,%eax,%xmm0",
        "popcnt %rax,%rax",
        "kmovw  %k0,%eax",
        "vptestnmd %ymm0,%ymm0,%k0",
    ] {
        assert!(above_baseline(above), "{above}");
    }
    for baseline in [
        "movd   %eax,%xmm0",
        "punpckldq %xmm1,%xmm0",
        "pextrw $0x1,%xmm0,%eax",
        "cpuid",
        "xgetbv",
        "movq   %rax,(%rdi)",
        "pxor   %xmm0,%xmm0",
    ] {
        assert!(!above_baseline(baseline), "{baseline}");
    }
}

/// A release program in four codegen units, `@noInline` functions too large to share one, each with
/// a CPU check that reads the feature word (the start-up check stores it in another).
fn several_units_program() -> String {
    let checks = ["Avx2", "Bmi2", "Popcnt", "Sse42", "Fma", "Lzcnt"];
    let mut src = format!(
        "import {{ noInline }} from \"velt/compiler\";\nimport {{ {} }} from \"velt/arch/x86\";\n",
        checks.join(", ")
    );
    for (i, check) in checks.iter().enumerate() {
        src.push_str(&format!(
            "@noInline\nfunction f{i}(n: i64): i64 {{\n  let x = n;\n"
        ));
        for k in 0..300 {
            src.push_str(&format!("  x = (x * {} + {}) % 1000003;\n", k + 3, k + i));
        }
        src.push_str(&format!("  return {check}.isSupported() ? x : n;\n}}\n"));
    }
    src.push_str("function main() {\n  let n: i64 = 0;\n");
    for i in 0..checks.len() {
        src.push_str(&format!("  n = f{i}(n);\n"));
    }
    src.push_str("  console.log(n >= 0);\n}\n");
    src
}

/// The objects in `dir` (`.o`, `.obj`) that name the feature word.
fn objects_with_the_word(dir: &Path) -> Vec<std::path::PathBuf> {
    (std::fs::read_dir(dir).unwrap().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "o" || e == "obj"))
        .filter(|p| {
            let bytes = std::fs::read(p).unwrap_or_default();
            bytes.windows(13).any(|w| w == b"velt_cpu_word")
        })
        .collect()
}

/// The feature word in several objects of one program links: every object that reads or sets it
/// defines it (`linkonce_odr`), in a comdat of its own on COFF, where link.exe refused two objects'
/// weak definitions (LNK1227, #3293 review). The program runs here with the word in two objects
/// or more; on Windows that is the link. Where `lld-link` is installed, the same program's
/// Windows objects are linked too, which reports the definitions' conflict as a duplicate symbol
/// (it did with the comdat left out).
#[test]
fn a_release_program_in_several_units_has_one_feature_word() {
    if !velt_codegen_llvm::available() || !cfg!(target_arch = "x86_64") {
        return;
    }
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    std::fs::write(root.join("main.velt"), several_units_program()).unwrap();
    let build = |out: &Path, target: &[&str]| {
        let mut args = vec!["build", "--release"];
        args.extend(target);
        args.extend(["main.velt", "-o", out.to_str().unwrap()]);
        no_window::command(VELT)
            .args(&args)
            .current_dir(root)
            .env("VELT_CODEGEN_UNITS", "4")
            .env_remove("VELT_CPU")
            .output()
            .unwrap()
    };
    let native = root.join("native");
    std::fs::create_dir_all(&native).unwrap();
    let exe = native.join(format!("app{}", std::env::consts::EXE_SUFFIX));
    let o = build(&exe, &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let ran = no_window::command(exe.to_str().unwrap()).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "true\n");
    let objects = objects_with_the_word(&native);
    assert!(objects.len() >= 2, "the word in only {objects:?}");
    // The Windows objects, linked by lld-link (the build's own link may fail here for want of
    // the target's runtime: the objects are written first).
    let lld = Command::new("lld-link").arg("--version").output();
    if cfg!(windows) || !lld.is_ok_and(|o| o.status.success()) {
        return;
    }
    let windows = root.join("windows");
    std::fs::create_dir_all(&windows).unwrap();
    build(
        &windows.join("app.exe"),
        &["--target", "x86_64-pc-windows-msvc"],
    );
    let objects = objects_with_the_word(&windows);
    assert!(objects.len() >= 2, "the word in only {objects:?}");
    let o = Command::new("lld-link")
        .args(["/dll", "/noentry", "/force:unresolved"])
        .arg(format!("/out:{}", windows.join("check.dll").display()))
        .args(&objects)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.contains("velt_cpu_word"), "{err}");
}
