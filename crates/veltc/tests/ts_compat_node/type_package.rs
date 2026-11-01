//! The `velt` type package (`velt doc --std --format types`) under the oracle's pinned `tsc`:
//! every public module's declarations compile under `strict`, a client's `import type` gets
//! the real types, and a client that misuses them is rejected. Like the Node samples it runs
//! only with `VELT_TSC_ORACLE` set (the nightly oracle job).

use std::path::Path;
use std::process::{Command, Output};

use super::super::{stderr, test_dir, velt};

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "strict": true,
    "noEmit": true,
    "module": "nodenext",
    "moduleResolution": "nodenext",
    "target": "esnext",
    "lib": ["esnext", "dom"],
    "types": []
  },
  "files": ["FILES"]
}
"#;

/// A client using the declarations as intended.
const CLIENT: &str = r#"import type { BTreeMap, Deque } from "velt/collections";
import type { readFile, MkdirOptions } from "velt/fs";
import type { Channel } from "velt/sync";

declare const map: BTreeMap<string, number>;
declare const q: Deque<string>;
declare const read: typeof readFile;
declare const ch: Channel<number>;

export async function demo(): Promise<string> {
  const opts: MkdirOptions = { recursive: true };
  const text: string = await read("a.txt");
  q.pushBack(text);
  const n: number | undefined = map.get("x");
  void opts;
  void n;
  void ch;
  return text;
}
"#;

/// A client that misuses them: `readFile` resolves to a string, a `Deque<string>` takes strings.
const MISUSE: &str = r#"import type { readFile } from "velt/fs";
import type { Deque } from "velt/collections";

declare const read: typeof readFile;
declare const q: Deque<string>;

export async function misuse(): Promise<void> {
  const n: number = await read("a.txt");
  q.pushBack(1);
  void n;
}
"#;

fn tsc(tsc: &Path, dir: &Path, config: &str) -> Output {
    Command::new("node")
        .arg(tsc)
        .args(["-p", config])
        .current_dir(dir)
        .output()
        .expect("run tsc")
}

#[test]
fn type_package_compiles_and_types_clients() {
    if std::env::var_os("VELT_TSC_ORACLE").is_none() {
        eprintln!("skipped: set VELT_TSC_ORACLE=1 to check the `velt` type package with tsc");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let tsc_bin = root.join("tests/tscompat-oracle/node_modules/typescript/bin/tsc");
    assert!(
        tsc_bin.is_file(),
        "VELT_TSC_ORACLE is set but {} is missing",
        tsc_bin.display()
    );
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let o = velt(
        dir,
        &[
            "doc",
            "--std",
            "--format",
            "types",
            "-o",
            "node_modules/velt",
        ],
    );
    assert!(
        o.status.success(),
        "velt doc --format types:\n{}",
        stderr(&o)
    );

    // Every module the package exports, imported as a namespace.
    let package: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("node_modules/velt/package.json")).unwrap(),
    )
    .unwrap();
    let exports = package["exports"].as_object().expect("an exports map");
    assert!(exports.len() > 20, "{} modules", exports.len());
    let all: String = exports
        .keys()
        .enumerate()
        .map(|(i, key)| {
            let spec = format!("velt{}", key.trim_start_matches('.'));
            format!("import type * as m{i} from \"{spec}\";\n")
        })
        .collect();
    std::fs::write(dir.join("all.ts"), all).unwrap();
    std::fs::write(dir.join("client.ts"), CLIENT).unwrap();
    std::fs::write(dir.join("misuse.ts"), MISUSE).unwrap();
    std::fs::write(
        dir.join("tsconfig.json"),
        TSCONFIG.replace("FILES", "all.ts\", \"client.ts"),
    )
    .unwrap();
    std::fs::write(
        dir.join("tsconfig.misuse.json"),
        TSCONFIG.replace("FILES", "misuse.ts"),
    )
    .unwrap();

    let o = tsc(&tsc_bin, dir, "tsconfig.json");
    assert!(
        o.status.success(),
        "tsc on the type package and a client:\n{}{}",
        String::from_utf8_lossy(&o.stdout),
        stderr(&o)
    );
    let o = tsc(&tsc_bin, dir, "tsconfig.misuse.json");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success(), "tsc accepted the misuse");
    for (line, code) in [(8, "TS2322"), (9, "TS2345")] {
        assert!(
            out.contains(&format!("misuse.ts({line},")) && out.contains(code),
            "expected {code} on line {line}:\n{out}"
        );
    }
}
