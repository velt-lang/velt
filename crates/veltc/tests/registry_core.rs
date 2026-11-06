//! packages/registry-core against the Rust it must agree with: its names against vpm's rules
//! (`vpm::manifest`, `velt_registry::auth`), its versions against the `semver` crate vpm uses,
//! and its archives against `vpm::archive`. tests/registry_core/driver/ runs registry-core over
//! the cases written here (a file, one case per line) and prints its answers; each is compared
//! with the Rust answer, and every disagreement is listed.
//!
//! The cases are listed edge cases plus generated ones: every short string over an alphabet that
//! covers each rule, versions assembled from every kind of part, every pair of an ordered list,
//! and every prefix of a valid archive.

#[path = "registry_core/drift.rs"]
mod drift;
#[path = "registry_core/manifests.rs"]
mod manifests;
mod no_window;
mod runtime_support;
// Only `root` and `copy_dir`.
#[allow(dead_code)]
mod wasm_tools;
mod work_dir;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use wasm_tools::{copy_dir, root};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(text: &str) -> String {
    let bytes: Vec<u8> = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex from the driver"))
        .collect();
    String::from_utf8(bytes).expect("UTF-8 from the driver")
}

/// One case: the line the driver reads, and the answer the Rust gives.
struct Case {
    line: String,
    want: String,
}

fn name_case(n: &str) -> Case {
    let flag = |b: bool| if b { "1" } else { "0" };
    let user = velt_registry::auth::check_user_name(n)
        .err()
        .unwrap_or_default();
    Case {
        line: format!("name\t{}", hex(n.as_bytes())),
        want: format!(
            "{}\t{}\t{}\t{}",
            flag(vpm::manifest::is_valid_package_name(n)),
            flag(vpm::manifest::is_valid_dependency_name(n)),
            flag(vpm::manifest::is_valid_user_name(n)),
            hex(user.as_bytes())
        ),
    }
}

fn version_case(v: &str) -> Case {
    Case {
        line: format!("version\t{}", hex(v.as_bytes())),
        want: match semver::Version::parse(v) {
            Ok(parsed) => format!("ok\t{}", hex(parsed.to_string().as_bytes())),
            Err(e) => format!("err\t{}", hex(e.to_string().as_bytes())),
        },
    }
}

fn compare_case(a: &str, b: &str) -> Case {
    let order = semver::Version::parse(a)
        .expect("a valid version")
        .cmp(&semver::Version::parse(b).expect("a valid version"));
    Case {
        line: format!("compare\t{}\t{}", hex(a.as_bytes()), hex(b.as_bytes())),
        want: (order as i8).to_string(),
    }
}

fn archive_case(archive: &[u8]) -> Case {
    Case {
        line: format!("archive\t{}", hex(archive)),
        want: match vpm::archive::checksum(archive) {
            Ok(sum) => format!("ok\t{}", hex(sum.as_bytes())),
            Err(e) => format!("err\t{}", hex(e.as_bytes())),
        },
    }
}

fn manifest_case(src: &str) -> Case {
    Case {
        line: format!("manifest\t{}", hex(src.as_bytes())),
        want: match vpm::manifest::Manifest::parse(src) {
            Ok(m) => format!("ok\t{}", hex(m.to_json().to_string().as_bytes())),
            Err(_) => "err".to_string(),
        },
    }
}

fn requirement_case(req: &str) -> Case {
    Case {
        line: format!("requirement\t{}", hex(req.as_bytes())),
        want: if semver::VersionReq::parse(req).is_ok() {
            "ok"
        } else {
            "err"
        }
        .to_string(),
    }
}

fn manifests() -> Vec<Case> {
    let mut all = manifests::whole();
    all.extend(manifests::mutations());
    all.extend(drift::keyword_cases(&root()));
    all.extend(drift::ts_compat_cases());
    all.extend(drift::big_cases());
    all.extend(drift::fuzz_cases(4000));
    all.iter().map(|m| manifest_case(m)).collect()
}

/// Requirements assembled from operators, versions with wildcards, and separators.
fn requirements() -> Vec<Case> {
    let ops = [
        "", "=", ">", ">=", "<", "<=", "~", "^", "= ", ">=  ", "=>", "~>", "!",
    ];
    let versions = [
        "1",
        "1.2",
        "1.2.3",
        "1.*",
        "1.x",
        "1.2.X",
        "1.*.3",
        "1.2.3-rc.1",
        "1.2-rc",
        "1.2.3+b",
        "01",
        "1.02",
        "*",
        "x",
        "1.2.3-01",
        "1.2.3-",
        "18446744073709551616",
        "",
    ];
    let mut reqs = vec![];
    for op in ops {
        for v in versions {
            reqs.push(format!("{op}{v}"));
        }
    }
    for r in [
        "*", " * ", "*, 1", "* 1", "x", "X", ">=1, <2", ">=1,<2", ">=1 ,<2", ">=1 <2", ">=1,",
        ",1", "1,,2", "1 || 2", "  ^1.2  ", "1.2.3 ", "	1", "1	", "1, x", "v1",
    ] {
        reqs.push(r.to_string());
    }
    let many = |n: usize| vec![">=1"; n].join(", ");
    reqs.push(many(32));
    reqs.push(many(33));
    reqs.iter().map(|r| requirement_case(r)).collect()
}

/// Every string of up to `len` characters over `alphabet`.
fn strings(alphabet: &[&str], len: usize) -> Vec<String> {
    let mut all = vec![String::new()];
    let mut last = vec![String::new()];
    for _ in 0..len {
        last = last
            .iter()
            .flat_map(|s| alphabet.iter().map(move |c| format!("{s}{c}")))
            .collect();
        all.extend(last.iter().cloned());
    }
    all
}

fn names() -> Vec<Case> {
    let mut names = strings(&["a", "z", "A", "0", "9", "-", "_", ".", "@", "/", "é"], 3);
    for n in [
        "std",
        "velt",
        "rt",
        "sig",
        "native",
        "rt-x",
        "rt_x",
        "rtx",
        "sigma",
        "native-ish",
        "con",
        "CON",
        "Con",
        "prn",
        "aux",
        "nul",
        "com0",
        "com9",
        "COM5",
        "lpt1",
        "lpt10",
        "com",
        "console",
        "auxiliary",
        "my-pkg",
        "my_pkg",
        "my--pkg",
        "my__pkg",
        "my-_pkg",
        "my_-pkg",
        "pkg-",
        "pkg_",
        "-pkg",
        "a1-b2_c3",
        "@sigx/reactivity",
        "@sigx/con",
        "@con/x",
        "@1x/y",
        "@x/y/z",
        "@x",
        "@/x",
        "@x/",
        "@X/y",
        "@x/y-",
        "@x--y/z",
        "ada",
        "a--b_",
        "user-1",
        "comé",
        "aé",
        "a\u{0}",
        "a\n",
        "a b",
        "ǅ",
    ] {
        names.push(n.to_string());
    }
    // Characters outside the Basic Multilingual Plane, and one in U+E000..U+FFFF.
    for n in ["a😀", "com😀", "@😀/x", "rt😀", "😀", "@x/😀", "a\u{ff61}"] {
        names.push(n.to_string());
    }
    names.iter().map(|n| name_case(n)).collect()
}

/// Versions assembled from numbers, pre-releases and build metadata of every kind.
fn version_texts() -> Vec<String> {
    let numbers = [
        "0",
        "1",
        "10",
        "01",
        "00",
        "",
        "x",
        "18446744073709551615",
        "18446744073709551616",
        "99999999999999999999",
    ];
    let pre = [
        None,
        Some(""),
        Some("alpha"),
        Some("1"),
        Some("01"),
        Some("0"),
        Some("a.b"),
        Some("a..b"),
        Some("a."),
        Some(".a"),
        Some("x-y"),
        Some("-"),
        Some("0a"),
        Some("a_b"),
        Some("é"),
        Some("1.0.0"),
        Some("99999999999999999999999"),
    ];
    let build = [
        None,
        Some(""),
        Some("001"),
        Some("a.b"),
        Some("a..b"),
        Some("b."),
        Some("x-1"),
        Some("+"),
        Some("é"),
    ];
    let mut texts = vec![];
    for major in numbers {
        for minor in ["0", "01", ""] {
            for patch in ["0", "7", "00"] {
                texts.push(format!("{major}.{minor}.{patch}"));
            }
        }
    }
    for p in pre {
        for b in build {
            let mut t = "1.2.3".to_string();
            if let Some(p) = p {
                t.push('-');
                t.push_str(p);
            }
            if let Some(b) = b {
                t.push('+');
                t.push_str(b);
            }
            texts.push(t);
        }
    }
    for t in [
        "", "1", "1.", "1.2", "1.2.", "v1.2.3", " 1.2.3", "1.2.3 ", "1.2.3.4", "1.2.3-", "1.2.3+",
        "1.2.3-+", "1.2.3_x", "1.2.3~", "١.2.3", "1.2.3\n",
    ] {
        texts.push(t.to_string());
    }
    // Characters the messages quote: escaped, invisible, outside the BMP.
    for t in [
        "1.2.3\0",
        "1.2.3\t",
        "1.2.3\r",
        "1.2.3'",
        "1.2.3\\",
        "1.2.3\u{301}",
        "1.2.3\u{a0}",
        "1.2.3😀",
        "1.2.3-a😀",
        "1.2.3+b\u{200b}",
        "\u{7f}.0.0",
        "1.\u{feff}.0",
        "1.2.3-\u{e000}",
    ] {
        texts.push(t.to_string());
    }
    texts
}

/// Valid versions in no particular order; every pair is compared.
const ORDERED: &[&str] = &[
    "1.0.0-alpha",
    "1.0.0-alpha.1",
    "1.0.0-alpha.beta",
    "1.0.0-beta",
    "1.0.0-beta.2",
    "1.0.0-beta.11",
    "1.0.0-rc.1",
    "1.0.0",
    "1.0.0-1",
    "1.0.0-10",
    "1.0.0-2",
    "1.0.0-a-b",
    "1.0.0-A",
    "1.0.0-Z",
    "1.0.0+0",
    "1.0.0+00",
    "1.0.0+1",
    "1.0.0+01",
    "1.0.0+001",
    "1.0.0+2",
    "1.0.0+10",
    "1.0.0+a",
    "1.0.0+1.a",
    "1.0.0+a.1",
    "1.0.0+1.0",
    "1.0.0-x+y",
    "1.0.0-x+z",
    "0.0.1",
    "0.1.0",
    "1.0.1",
    "1.1.0",
    "2.0.0",
    "10.0.0",
    "18446744073709551615.0.0",
];

/// More build metadata and pre-releases, compared with each other and with [`ORDERED`].
const MORE_ORDERED: &[&str] = &[
    "1.0.0+0a",
    "1.0.0+a0",
    "1.0.0+-",
    "1.0.0+1.1",
    "1.0.0+1.01",
    "1.0.0-0",
    "1.0.0--",
    "1.0.0-a.b.c",
    "1.0.0-alpha.1+x",
];

fn versions() -> Vec<Case> {
    let mut cases: Vec<Case> = version_texts().iter().map(|v| version_case(v)).collect();
    let all: Vec<&str> = ORDERED.iter().chain(MORE_ORDERED).copied().collect();
    for a in &all {
        for b in &all {
            cases.push(compare_case(a, b));
        }
    }
    cases
}

fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"VELTPKG1\n".to_vec();
    for (path, bytes) in files {
        out.extend_from_slice(format!("{path}\n{}\n", bytes.len()).as_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

fn archives() -> Vec<Case> {
    let manifest: &[u8] = b"export const pkg: Package = { name: \"p\", version: \"1.0.0\" };\n";
    let valid = archive(&[
        ("package.velt", manifest),
        ("src/lib.velt", b"export function f(): i64 { return 1; }\n"),
        ("src/sub/\u{e9}.velt", b""),
        ("src/b.velt", &[0, 255, 10, 13]),
    ]);
    let mut all = vec![valid.clone()];
    // Every prefix of a valid archive: truncated files, missing newlines, bad lengths.
    for end in 0..valid.len() {
        all.push(valid[..end].to_vec());
    }
    for path in [
        "../x",
        "/etc/passwd",
        "src/../../x",
        "src//x",
        "src/./x",
        "./src/x",
        "src/x/",
        "a\\b",
        "c:x",
        "target/x",
        "native/x",
        "velt.toml",
        "src",
        "package.velt/x",
        "",
    ] {
        all.push(archive(&[("package.velt", manifest), (path, b"x")]));
    }
    for length in [
        "+3",
        "003",
        "-3",
        " 3",
        "3 ",
        "0x3",
        "",
        "99999999999999999999999",
    ] {
        let mut a = b"VELTPKG1\npackage.velt\n".to_vec();
        a.extend_from_slice(format!("{length}\nabc").as_bytes());
        all.push(a);
    }
    for length in ["+0", "+", "-0", "0000000000000000000000001"] {
        let mut a = b"VELTPKG1\npackage.velt\n".to_vec();
        a.extend_from_slice(format!("{length}\na").as_bytes());
        all.push(a);
    }
    // A length line that isn't UTF-8.
    all.push([&b"VELTPKG1\nsrc/a\n"[..], &[0xff], b"\n"].concat());
    // Paths whose UTF-16 order differs from their byte order, a NUL in a path, an empty last file.
    all.push(archive(&[
        ("package.velt", manifest),
        ("src/\u{1f600}.velt", b"a"),
        ("src/\u{ff61}.velt", b"b"),
        ("src/\u{e000}", b"c"),
        ("src/a\u{0}b", b"d"),
        ("src/z", b""),
    ]));
    // A native crate directory: allowed only when the manifest declares it.
    let native: &[u8] =
        b"export const pkg: Package = { name: \"p\", version: \"1.0.0\", native: { path: \"crate\" } };";
    all.push(archive(&[
        ("crate/Cargo.toml", b""),
        ("package.velt", native),
    ]));
    all.push(archive(&[
        ("native/Cargo.toml", b""),
        ("package.velt", native),
    ]));
    all.push(archive(&[
        ("native/Cargo.toml", b""),
        (
            "package.velt",
            b"export const pkg: Package = { name: \"p\", version: \"1.0.0\", native: {} };",
        ),
    ]));
    // Every prefix cutting a 4-byte character.
    let wide = archive(&[("package.velt", manifest), ("src/\u{1f600}", b"x")]);
    for end in wide.len() - 8..wide.len() {
        all.push(wide[..end].to_vec());
    }
    let mut bad_utf8 = b"VELTPKG1\nsrc/".to_vec();
    bad_utf8.extend_from_slice(&[0xff, b'\n', b'0', b'\n']);
    all.push(bad_utf8);
    all.push(b"VELTPKG2\n".to_vec());
    all.push(b"VELTPKG1".to_vec());
    // The same path twice, and files out of order: both are read as they come.
    all.push(archive(&[
        ("src/a.velt", b"1"),
        ("src/a.velt", b"2"),
        ("package.velt", manifest),
    ]));
    all.iter().map(|a| archive_case(a)).collect()
}

/// Builds the driver in a work directory with packages/registry-core beside it.
fn driver() -> (PathBuf, PathBuf) {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let work = work_dir::work_dir(&root, "registry-core-work").join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&work);
    copy_dir(&root.join("crates/veltc/tests/registry_core"), &work);
    copy_dir(
        &root.join("packages/registry-core"),
        &work.join("registry-core"),
    );
    (root, work)
}

fn run(work: &Path, cases: &[Case]) -> Vec<String> {
    let file = work.join("cases.txt");
    let text: String = cases.iter().map(|c| format!("{}\n", c.line)).collect();
    std::fs::write(&file, text).expect("write the cases");
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "src/main.velt"])
        .arg(&file)
        .current_dir(work.join("driver"))
        .output()
        .expect("run velt");
    assert!(
        o.status.success(),
        "the driver failed:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// A manifest answer's JSON written as `serde_json` writes the Rust side's (object keys sorted),
/// so the two compare as values.
fn normalize_manifest(line: &str, got: String) -> String {
    if !line.starts_with("manifest\t") {
        return got;
    }
    let Some(json) = got.strip_prefix("ok\t") else {
        return got;
    };
    match serde_json::from_str::<serde_json::Value>(&unhex(json)) {
        Ok(v) => format!("ok\t{}", hex(v.to_string().as_bytes())),
        Err(e) => format!("bad-json\t{}", hex(e.to_string().as_bytes())),
    }
}

/// The cases whose answers differ, readably.
fn disagreements(cases: &[Case], got: &[String]) -> Vec<String> {
    assert_eq!(got.len(), cases.len(), "one answer per case");
    let show = |s: &str| {
        s.split('\t')
            .map(|f| {
                let decodable =
                    f.len() % 2 == 0 && f.bytes().all(|b| b.is_ascii_hexdigit()) && f.len() > 2;
                if decodable {
                    format!("{:?}", unhex(f))
                } else {
                    f.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    cases
        .iter()
        .zip(got)
        .filter(|(c, g)| c.want != **g)
        .map(|(c, g)| {
            format!(
                "{}\n  rust:  {}\n  velt:  {}",
                show(&c.line),
                show(&c.want),
                show(g)
            )
        })
        .collect()
}

#[test]
fn registry_core_agrees_with_vpm() {
    let (_, work) = driver();
    let mut cases = names();
    let counts = [cases.len()];
    cases.extend(versions());
    cases.extend(archives());
    cases.extend(requirements());
    cases.extend(manifests());
    let got: Vec<String> = run(&work, &cases)
        .into_iter()
        .zip(&cases)
        .map(|(g, c)| normalize_manifest(&c.line, g))
        .collect();
    let wrong = disagreements(&cases, &got);
    assert!(
        wrong.is_empty(),
        "{} of {} cases disagree ({} names first):\n{}",
        wrong.len(),
        cases.len(),
        counts[0],
        wrong
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    eprintln!("{} cases agree", cases.len());
    let _ = std::fs::remove_dir_all(&work);
}
