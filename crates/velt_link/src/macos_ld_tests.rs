//! The direct `ld` link's pieces on stand-ins: the driver's printed command, a link's arguments
//! as `ld` takes them, and the kept template (asked once, asked again for another toolchain).

use super::*;

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

#[test]
fn applies_on_macos_unless_the_linker_is_overridden() {
    use crate::TargetOs;
    assert!(applies(TargetOs::MacOs, false));
    assert!(!applies(TargetOs::MacOs, true));
    assert!(!applies(TargetOs::Linux, false));
    assert!(!applies(TargetOs::Windows, false));
}

/// `cc -###` quotes every argument; a quote or a backslash inside comes after a backslash.
#[test]
fn the_driver_s_printed_command_is_read_back() {
    let line = r#" "/usr/bin/ld" "-demangle" "-o" "a b" "say \"hi\"" "back\\slash""#;
    assert_eq!(
        quoted(line).unwrap(),
        [
            "/usr/bin/ld",
            "-demangle",
            "-o",
            "a b",
            "say \"hi\"",
            "back\\slash"
        ]
    );
    assert_eq!(quoted("").unwrap(), Vec::<String>::new());
    // Not a quoted command line: an unquoted word, a quote left open.
    assert_eq!(quoted(r#" "/usr/bin/ld" -demangle"#), None);
    assert_eq!(quoted(r#" "/usr/bin/ld" "-o"#), None);
}

/// The command Apple clang 21 prints for a link (the command line tools' paths).
fn printed(input: &str) -> Vec<String> {
    strings(&[
        "/Library/Developer/CommandLineTools/usr/bin/ld",
        "-demangle",
        "-lto_library",
        "/Library/Developer/CommandLineTools/usr/lib/libLTO.dylib",
        "-dynamic",
        "-arch",
        "arm64",
        "-platform_version",
        "macos",
        "11.0.0",
        "27.0",
        "-syslibroot",
        "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk",
        "-mllvm",
        "-enable-linkonceodr-outlining",
        "-o",
        OUTPUT,
        "-L/usr/local/lib",
        input,
        "-lSystem",
        "/Library/Developer/CommandLineTools/usr/lib/clang/21/lib/darwin/libclang_rt.osx.a",
    ])
}

#[test]
fn the_template_is_the_driver_s_command_without_the_link_s_own_arguments() {
    let t = Template::of(&printed("/usr/bin/cc"), "/usr/bin/cc").unwrap();
    assert_eq!(
        t.program,
        Path::new("/Library/Developer/CommandLineTools/usr/bin/ld")
    );
    assert_eq!(t.before.first().map(String::as_str), Some("-demangle"));
    assert_eq!(
        t.before.last().map(String::as_str),
        Some("-enable-linkonceodr-outlining")
    );
    assert_eq!(t.between, ["-L/usr/local/lib"]);
    assert_eq!(t.after.first().map(String::as_str), Some("-lSystem"));
    assert_eq!(t.after.len(), 2);
}

/// Anything but clang's `ld` command with the output and the input where they are expected
/// gives no template (the link goes through `cc`).
#[test]
fn another_command_gives_no_template() {
    let input = "/usr/bin/cc";
    let mut gcc = printed(input);
    gcc[0] = "/usr/libexec/gcc/collect2".into();
    assert_eq!(Template::of(&gcc, input), None);
    let mut relative = printed(input);
    relative[0] = "ld".into();
    assert_eq!(Template::of(&relative, input), None);
    assert_eq!(Template::of(&printed("/other"), input), None);
    let no_output: Vec<String> = (printed(input).into_iter())
        .filter(|a| a != OUTPUT)
        .collect();
    assert_eq!(Template::of(&no_output, input), None);
    let mut twice = printed(input);
    twice.push(input.into());
    assert_eq!(Template::of(&twice, input), None);
    assert_eq!(Template::of(&[], input), None);
}

/// The two links `velt` does, as `ld` takes them: the driver's own options go, the linker's
/// (`-Wl,`) are split, and the `-L` directories are apart (they go before the driver's).
#[test]
fn a_link_s_arguments_as_ld_takes_them() {
    let shared = for_ld(&os(&[
        "-arch",
        "arm64",
        "-mmacosx-version-min=11.0",
        "a.o",
        "a.entry.o",
        "-L/t/debug",
        "-lvelt_rt_shared",
        "-Wl,-rpath,/t/debug",
        "-o",
        "/out/a",
    ]))
    .unwrap();
    assert_eq!(
        shared,
        Link {
            arch: "arm64".into(),
            min_version: "11.0".into(),
            output: "/out/a".into(),
            search: os(&["-L/t/debug"]),
            rest: os(&["a.o", "a.entry.o", "-lvelt_rt_shared", "-rpath", "/t/debug"]),
        }
    );
    let release = for_ld(&os(&[
        "-arch",
        "x86_64",
        "-mmacosx-version-min=10.12",
        "-Wl,-dead_strip",
        "a.o",
        "/t/libvelt_rt.a",
        "-liconv",
        "-lc",
        "-lm",
        "-framework",
        "Security",
        "-o",
        "a",
    ]))
    .unwrap();
    assert_eq!(release.arch, "x86_64");
    assert_eq!(
        release.rest,
        os(&[
            "-dead_strip",
            "a.o",
            "/t/libvelt_rt.a",
            "-liconv",
            "-lc",
            "-lm",
            "-framework",
            "Security"
        ])
    );
    assert!(release.search.is_empty());
}

/// An argument this does not know, or a link without an architecture, a deployment target or
/// an output, is not translated: the link goes through `cc`.
#[test]
fn an_unknown_argument_leaves_the_link_to_cc() {
    let base = [
        "-arch",
        "arm64",
        "-mmacosx-version-min=11.0",
        "a.o",
        "-o",
        "a",
    ];
    assert!(for_ld(&os(&base)).is_some());
    for extra in ["-pthread", "-fuse-ld=lld", "-O2", "-shared", "-l", "-L"] {
        let mut args = base.to_vec();
        args.insert(3, extra);
        assert_eq!(for_ld(&os(&args)), None, "{extra}");
    }
    assert_eq!(
        for_ld(&os(&["-mmacosx-version-min=11.0", "a.o", "-o", "a"])),
        None
    );
    assert_eq!(for_ld(&os(&["-arch", "arm64", "a.o", "-o", "a"])), None);
    assert_eq!(
        for_ld(&os(&["-arch", "arm64", "-mmacosx-version-min=11.0", "a.o"])),
        None
    );
    assert_eq!(
        for_ld(&os(&[
            "-arch",
            "arm64",
            "-mmacosx-version-min=11.0",
            "a.o",
            "-o"
        ])),
        None
    );
}

/// The driver is the first `cc` on the search path, and only where that is in a directory given
/// in full: one in the working directory (an empty or relative entry) is not asked, and neither
/// is the next one after it (the link goes through `cc`, as the system finds it): `on_path`
/// gives the first one found only when its path is given in full.
#[test]
fn the_driver_is_the_first_cc_and_never_the_working_directory_s() {
    let root = std::env::temp_dir().join(format!("velt-macos-ld-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (a, b) = (root.join("a"), root.join("b"));
    for d in [&a, &b] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(d.join("cc"), b"").unwrap();
    }
    let path = |dirs: &[&Path]| std::env::join_paths(dirs).unwrap();
    assert_eq!(on_path(&path(&[&root, &b, &a]), "cc"), Some(b.join("cc")));
    assert_eq!(on_path(&path(&[&root]), "cc"), None);
    // A relative entry that holds no `cc` (resolved against the directory the tests run in) is
    // passed over like any other.
    let nowhere = Path::new("velt-macos-ld-no-such-directory");
    assert_eq!(on_path(&path(&[nowhere, &a]), "cc"), Some(a.join("cc")));
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
mod kept {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A directory with a stand-in `ld` and a stand-in driver that counts its runs and prints
    /// what `print` says for a link (`$5` is the input it was asked with).
    struct Standin {
        dir: PathBuf,
        driver: PathBuf,
        ld: PathBuf,
        cache: PathBuf,
    }

    impl Standin {
        fn new(test: &str, print: &str) -> Standin {
            let dir =
                std::env::temp_dir().join(format!("velt-macos-ld-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let ld = dir.join("ld");
            std::fs::write(&ld, b"ld 1").unwrap();
            let driver = dir.join("cc");
            let script = format!(
                "#!/bin/sh\necho run >> '{}'\necho 'Apple clang version 21' >&2\n{}\n",
                dir.join("runs").display(),
                print.replace("LD", &ld.display().to_string())
            );
            std::fs::write(&driver, script).unwrap();
            std::fs::set_permissions(&driver, std::fs::Permissions::from_mode(0o755)).unwrap();
            let cache = dir.join("cache");
            Standin {
                dir,
                driver,
                ld,
                cache,
            }
        }

        fn runs(&self) -> usize {
            std::fs::read_to_string(self.dir.join("runs")).map_or(0, |t| t.lines().count())
        }

        fn direct(&self, args: &[&str]) -> Option<Direct> {
            direct_with(&os(args), &self.driver, &self.cache)
        }

        fn command(&self, args: &[&str]) -> Option<Vec<String>> {
            let cmd = self.direct(args)?.command;
            let mut line = vec![cmd.get_program().to_string_lossy().into_owned()];
            line.extend(cmd.get_args().map(|a| a.to_string_lossy().into_owned()));
            Some(line)
        }

        /// The files kept in the cache.
        fn kept(&self) -> Vec<PathBuf> {
            let Ok(dir) = std::fs::read_dir(&self.cache) else {
                return vec![];
            };
            dir.map(|e| e.unwrap().path()).collect()
        }
    }

    impl Drop for Standin {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// What clang prints, cut down: the `ld` command on standard error.
    const CLANG: &str = r#"echo " \"LD\" \"-demangle\" \"-syslibroot\" \"/sdk\" \"-o\" \"velt-ld-template-output\" \"-L/usr/local/lib\" \"$5\" \"-lSystem\" \"/clang/libclang_rt.osx.a\"" >&2"#;

    const LINK: [&str; 9] = [
        "-arch",
        "arm64",
        "-mmacosx-version-min=11.0",
        "a.o",
        "-L/t/debug",
        "-lvelt_rt_shared",
        "-Wl,-rpath,/t/debug",
        "-o",
        "/out/a",
    ];

    /// The driver is asked once; the link is `ld` with the driver's arguments around the link's
    /// own, the link's `-L` directories before the driver's. A later link, of another program,
    /// asks nothing.
    #[test]
    fn the_driver_is_asked_once_and_ld_gets_its_arguments() {
        let s = Standin::new("once", CLANG);
        let line = s.command(&LINK).expect("a direct link");
        let ld = s.ld.display().to_string();
        assert_eq!(
            line,
            [
                ld.as_str(),
                "-demangle",
                "-syslibroot",
                "/sdk",
                "-o",
                "/out/a",
                "-L/t/debug",
                "-L/usr/local/lib",
                "a.o",
                "-lvelt_rt_shared",
                "-rpath",
                "/t/debug",
                "-lSystem",
                "/clang/libclang_rt.osx.a",
            ]
        );
        assert_eq!(s.runs(), 1);
        let mut other = LINK;
        other[3] = "b.o";
        other[8] = "/out/b";
        let line = s.command(&other).expect("a direct link");
        assert!(line.contains(&"b.o".to_string()) && line.contains(&"/out/b".to_string()));
        assert_eq!(s.runs(), 1);
        // Another deployment target is another link for the driver.
        let mut newer = LINK;
        newer[2] = "-mmacosx-version-min=14.0";
        assert!(s.command(&newer).is_some());
        assert_eq!(s.runs(), 2);
    }

    /// An updated toolchain (`ld` changed) asks the driver again.
    #[test]
    fn a_changed_ld_asks_the_driver_again() {
        let s = Standin::new("changed", CLANG);
        assert!(s.command(&LINK).is_some());
        assert!(s.command(&LINK).is_some());
        assert_eq!(s.runs(), 1);
        std::fs::write(&s.ld, b"ld 2, longer").unwrap();
        assert!(s.command(&LINK).is_some());
        assert_eq!(s.runs(), 2);
        // And one that is gone gives no direct link until the driver names another.
        std::fs::remove_file(&s.ld).unwrap();
        let _ = s.command(&LINK);
        assert_eq!(s.runs(), 3);
    }

    /// A driver that is not clang (no `ld` command printed) gives no direct link, and is not
    /// asked at every link either.
    #[test]
    fn a_driver_without_an_ld_command_is_asked_once_too() {
        let s = Standin::new("gcc", r#"echo ' "/usr/libexec/collect2" "-o" "x"' >&2"#);
        assert_eq!(s.command(&LINK), None);
        assert_eq!(s.command(&LINK), None);
        assert_eq!(s.runs(), 1);
    }

    /// A direct link that failed forgets what was kept: the next link asks the driver again,
    /// where a kept template gone stale would fail before every link's `cc`.
    #[test]
    fn a_failed_direct_link_forgets_the_template() {
        let s = Standin::new("forget", CLANG);
        let direct = s.direct(&LINK).expect("a direct link");
        assert_eq!((s.runs(), s.kept().len()), (1, 1));
        direct.kept.forget();
        assert!(s.kept().is_empty());
        assert!(s.command(&LINK).is_some());
        assert_eq!((s.runs(), s.kept().len()), (2, 1));
        // Forgetting twice, or what another link wrote again, is no error.
        direct.kept.forget();
        direct.kept.forget();
    }

    /// A kept file that was not written whole is not read: cut anywhere, at a line's end too,
    /// it lacks the end marker, and the driver is asked again.
    #[test]
    fn a_file_cut_short_is_not_read() {
        let s = Standin::new("cut", CLANG);
        assert!(s.command(&LINK).is_some());
        let file = s.kept().pop().expect("a kept file");
        let whole = std::fs::read_to_string(&file).unwrap();
        assert!(whole.ends_with("end\n"), "{whole}");
        let lines: Vec<&str> = whole.lines().collect();
        // Without its last argument lines (still a header, a key, a stamp and a program).
        let cut = lines[..lines.len() - 3].join("\n") + "\n";
        std::fs::write(&file, &cut).unwrap();
        let line = s.command(&LINK).expect("a direct link");
        assert_eq!(
            line.last().map(String::as_str),
            Some("/clang/libclang_rt.osx.a")
        );
        assert_eq!(s.runs(), 2);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), whole);
        std::fs::write(&file, &whole[..whole.len() - 2]).unwrap();
        assert!(s.command(&LINK).is_some());
        assert_eq!(s.runs(), 3);
    }

    /// Links at once, in one process, each keep a whole file (the same one): every write has a
    /// temporary name of its own, and none is left behind.
    #[test]
    fn links_at_once_keep_a_whole_file() {
        let s = Standin::new("at-once", CLANG);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..5 {
                        let direct = s.direct(&LINK).expect("a direct link");
                        direct.kept.forget();
                    }
                });
            }
        });
        assert!(s.command(&LINK).is_some());
        let kept = s.kept();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(std::fs::read_to_string(&kept[0])
            .unwrap()
            .ends_with("end\n"));
    }

    /// A link with an argument `ld` is not given asks nothing and makes nothing.
    #[test]
    fn an_untranslated_link_asks_nothing() {
        let s = Standin::new("unknown", CLANG);
        let mut link = LINK.to_vec();
        link.insert(3, "-pthread");
        assert_eq!(s.command(&link), None);
        assert_eq!(s.runs(), 0);
        assert!(!s.cache.exists());
    }
}
