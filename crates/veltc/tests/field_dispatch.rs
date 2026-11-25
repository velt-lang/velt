//! The field operations' fast x86-64 form (rt_abi.md "Field arithmetic") is chosen by the CPU
//! feature word: a release build run with `$VELT_CPU_DISABLE` naming ADX or BMI2 takes the other
//! form (velt/arch/x86's `Adx.isSupported()`, which reads the same bits, says false), and every
//! form gives the same X25519 and P-256 results, RFC 7748's vector among them. On an x86-64 host
//! with BMI2 and ADX; elsewhere there is one form and nothing to choose.

use std::path::Path;
use std::process::Output;

mod no_window;
mod test_dir;

const VELT: &str = env!("CARGO_BIN_EXE_velt");

const PROGRAM: &str = r#"import { Adx } from "velt/arch/x86";
import { p256Ecdh, p256PublicKey, x25519 } from "velt/crypto";
import { hexDecode, hexEncode } from "velt/encoding";

function main() {
  console.log(`adx ${Adx.isSupported()}`);
  // RFC 7748 §5.2's first vector.
  const k = hexDecode("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
  const u = hexDecode("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
  console.log(hexEncode(x25519(k, u)));
  // A P-256 public key and an agreement with it (field operations and the fused doubling).
  const a = hexDecode("c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721");
  const b = hexDecode("0f56db78ca460b055c500064824bed999a25aaf48ebb519ac201537b85479813");
  const pa = p256PublicKey(a);
  console.log(hexEncode(pa));
  console.log(hexEncode(p256Ecdh(b, pa)));
}
"#;

fn run(exe: &Path, disable: Option<&str>) -> String {
    let mut cmd = no_window::command(exe.to_str().unwrap());
    cmd.env_remove("VELT_CPU_DISABLE");
    if let Some(d) = disable {
        cmd.env("VELT_CPU_DISABLE", d);
    }
    let o: Output = cmd.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn velt_cpu_disable_selects_the_field_operations_fallback() {
    #[cfg(target_arch = "x86_64")]
    let adx =
        std::arch::is_x86_feature_detected!("bmi2") && std::arch::is_x86_feature_detected!("adx");
    #[cfg(not(target_arch = "x86_64"))]
    let adx = false;
    if !adx {
        eprintln!("note: not an x86-64 CPU with BMI2 and ADX; nothing to choose between");
        return;
    }
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    std::fs::write(root.join("main.velt"), PROGRAM).unwrap();
    let exe = root.join(if cfg!(windows) { "app.exe" } else { "app" });
    let o = no_window::command(VELT)
        .args([
            "build",
            "--release",
            "main.velt",
            "-o",
            exe.to_str().unwrap(),
        ])
        .current_dir(root)
        .env_remove("VELT_CPU")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let fast = run(&exe, None);
    let mut lines = fast.lines();
    assert_eq!(lines.next(), Some("adx true"), "{fast}");
    assert_eq!(
        lines.next(),
        Some("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552"),
        "RFC 7748 §5.2"
    );
    let results: Vec<&str> = fast.lines().skip(1).collect();
    for disable in ["ADX", "BMI2", "adx,bmi2"] {
        let slow = run(&exe, Some(disable));
        let mut lines = slow.lines();
        assert_eq!(lines.next(), Some("adx false"), "{disable}: {slow}");
        assert_eq!(lines.collect::<Vec<_>>(), results, "{disable}");
    }
}
