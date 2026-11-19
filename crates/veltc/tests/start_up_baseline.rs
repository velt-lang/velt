//! std's start-up check of the CPU (rt_abi.md "CPU check") runs before anything knows what the
//! CPU has, so it must use no instruction beyond the architecture's baseline, whatever `--cpu`
//! says: on a CPU without the build's features it must print its message, not die of an illegal
//! instruction in its own code (#3293 review). `velt_vir::start_up` marks the check and what it
//! reaches, and both backends compile those functions for the baseline. This disassembles them in
//! objects for x86-64-v4 and v3, on any host with `objdump` (no CPU or emulator needed), and finds
//! no VEX or EVEX encoding, no wide or mask register, and nothing from SSE3 up or BMI.

use std::path::Path;
use std::process::Command;

mod no_window;
mod test_dir;

const VELT: &str = env!("CARGO_BIN_EXE_velt");
const PROGRAM: &str = "function main() {\n  console.log(\"started\");\n}\n";

/// The functions of the start-up check, by their symbols (std/lang/hooks/cpu.velt, std/lang/cpu).
fn is_start_up(symbol: &str) -> bool {
    symbol.starts_with("_V3stdP4langP3cpu") || symbol.starts_with("_V3stdP4langP5hooksP3cpu")
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

/// The start-up functions in `object` and their instructions above the baseline, by objdump; `None`
/// where no objdump runs here.
fn disassembled(object: &Path) -> Option<(usize, Vec<String>)> {
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
            current = is_start_up(name.1).then(|| name.1.to_string());
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
    for (i, (what, flags)) in builds.iter().enumerate() {
        let object = root.join(format!("start{i}.o"));
        let mut args = vec![
            "build",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--emit",
            "obj",
        ];
        args.extend(flags);
        args.extend(["-o", object.to_str().unwrap(), "main.velt"]);
        let o = no_window::command(VELT)
            .args(&args)
            .current_dir(root)
            .env_remove("VELT_CPU")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{what}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let Some((functions, bad)) = disassembled(&object) else {
            eprintln!("no objdump here: the start-up check's instructions go unchecked");
            return;
        };
        assert!(functions > 0, "{what}: no start-up function in the object");
        assert!(
            bad.is_empty(),
            "{what}: above the baseline:\n{}",
            bad.join("\n")
        );
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
