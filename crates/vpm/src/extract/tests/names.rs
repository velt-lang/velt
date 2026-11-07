//! Paths every OS reads the same way, and clashes.

use super::*;

fn tar_with(name: &str) -> Vec<u8> {
    TarBuilder::default().file(name, b"x").gz()
}

#[test]
fn a_name_of_only_ignorable_code_points_is_refused() {
    // Its clash key is empty, so the raw name and an ordinary one would share keys, and a link
    // through it would be missed: `\u{200c}/a/l` -> `../..`, then `x` -> `\u{200c}/a/l/../..`.
    let tar = TarBuilder::default()
        .file("\u{200c}/a/f", b"x")
        .symlink("\u{200c}/a/l", "../..")
        .symlink("x", "\u{200c}/a/l/../..")
        .gz();
    refused(&tar, "code points macOS ignores");
}

#[test]
fn paths_outside_the_tree_are_refused() {
    refused(&tar_with("/etc/passwd"), "an absolute path");
    refused(&tar_with("../evil"), "a `..` segment");
    refused(&tar_with("a/../../evil"), "a `..` segment");
    refused(&tar_with("a/.."), "a `..` segment");
    refused(&tar_with("a//b"), "an empty path segment");
    // Through a pax header too, which is not cut at 100 bytes.
    let pax = TarBuilder::default()
        .pax(b'x', &[("path", "../../evil")])
        .file("harmless", b"x")
        .gz();
    refused(&pax, "a `..` segment");
}

#[test]
fn dot_segments_and_the_root_are_dropped() {
    let tar = TarBuilder::default()
        .dir("./")
        .file("./a/./b.txt", b"x")
        .dir("c/")
        .gz();
    assert_eq!(
        nodes(&tar),
        [
            ("a".to_string(), Node::Dir),
            ("a/b.txt".to_string(), Node::File(b"x".to_vec())),
            ("c".to_string(), Node::Dir),
        ]
    );
    // The root as a file is not a tree.
    refused(&tar_with("./"), "the archive's root must be a directory");
}

#[test]
fn names_windows_reads_differently_are_refused() {
    for (name, why) in [
        ("a\\b", "`\\` in a name"),
        ("c:x", "`:` in a name"),
        ("file.txt:stream", "alternate stream"),
        ("a<b", "`<` in a name"),
        ("q?", "`?` in a name"),
        ("CON", "a device name"),
        ("dir/nul.txt", "a device name"),
        ("Com1.tar.gz", "a device name"),
        ("lpt9", "a device name"),
        ("COM¹", "a device name"),
        ("conout$", "a device name"),
        ("trailing.", "ending in a dot or a space"),
        ("dir /x", "ending in a dot or a space"),
        ("tab\there", "a control character"),
        ("PROGRA~1", "a Windows short (8.3) name"),
        ("dir/ABCDEF~12.TXT", "a Windows short (8.3) name"),
    ] {
        let e = refusal(&tar_with(name));
        assert!(e.contains(why), "{name:?}: expected {why:?} in {e}");
    }
    // Close to device names, but ordinary.
    for name in [
        "console",
        "COM10",
        "nul_",
        "conx.txt",
        "lpt",
        ".hidden",
        "a..b",
        "backup~",
        "abcdefg~1",
        "a~b",
        "x~1.json",
    ] {
        assert_eq!(nodes(&tar_with(name)).len(), 1, "{name}");
    }
}

#[test]
fn deep_paths_are_refused() {
    // Through pax: a tar name field holds 100 bytes.
    let pax = |path: &str| {
        TarBuilder::default()
            .pax(b'x', &[("path", path)])
            .file("x", b"")
            .gz()
    };
    refused(
        &pax(&vec!["d"; 129].join("/")),
        "more than 128 directories deep",
    );
    assert_eq!(nodes(&pax(&vec!["d"; 128].join("/"))).len(), 128);
}

#[test]
fn a_nul_in_a_name_is_refused() {
    // A tar name ends at its first NUL; pax and zip names can hold one.
    let pax = TarBuilder::default()
        .pax(b'x', &[("path", "a\0b")])
        .file("x", b"")
        .gz();
    refused(&pax, "a NUL in a name");
    let mut entry = ZipEntry::file("ab", b"");
    entry.name = b"a\0b".to_vec();
    refused(&zip(&[entry], false), "a NUL in a name");
}

#[test]
fn names_that_differ_only_by_case_or_normalisation_are_refused() {
    let case = TarBuilder::default()
        .file("README", b"1")
        .file("readme", b"2")
        .gz();
    refused(&case, "differ only by case");
    // A directory too: `Src/a` and `src/b` would share one directory on macOS.
    let dirs = TarBuilder::default()
        .file("Src/a", b"1")
        .file("src/b", b"2")
        .gz();
    refused(&dirs, "`Src` and `src` differ only by case");
    // é composed (NFC) and decomposed (NFD).
    let unicode = TarBuilder::default()
        .file("caf\u{e9}", b"1")
        .file("cafe\u{301}", b"2")
        .gz();
    refused(&unicode, "Unicode normalisation");
    // Letters lowercasing alone keeps apart, which APFS takes for one name.
    for (a, b) in [("\u{17f}rc", "src"), ("\u{3c2}", "\u{3c3}")] {
        let tar = TarBuilder::default().file(a, b"1").file(b, b"2").gz();
        refused(&tar, "differ only by case");
    }
    let mut nfd = ZipEntry::file("", b"2");
    nfd.name = "Cafe\u{301}".as_bytes().to_vec();
    nfd.flags = 1 << 11;
    let mut nfc = nfd.clone();
    nfc.name = "caf\u{e9}".as_bytes().to_vec();
    refused(&zip(&[nfc, nfd], false), "Unicode normalisation");
}

#[test]
fn an_entry_written_twice_is_refused() {
    let tar = TarBuilder::default()
        .file("a.txt", b"first")
        .file("a.txt", b"second")
        .gz();
    refused(&tar, "holds this path twice");
    let zip = zip(
        &[ZipEntry::file("a", b"1"), ZipEntry::file("a", b"2")],
        false,
    );
    refused(&zip, "holds this path twice");
    // A directory listed again is harmless.
    let dirs = TarBuilder::default()
        .dir("d/")
        .file("d/x", b"")
        .dir("d/")
        .gz();
    assert_eq!(nodes(&dirs).len(), 2);
}

#[test]
fn a_path_that_is_a_file_and_a_directory_is_refused() {
    let file_first = TarBuilder::default().file("a", b"").file("a/b", b"").gz();
    refused(&file_first, "`a` is a file, so nothing can be under it");
    let dir_first = TarBuilder::default().file("a/b", b"").file("a", b"").gz();
    refused(&dir_first, "`a` is a directory and also a file or link");
}

#[test]
fn zip_names_must_be_utf8() {
    let mut cp437 = ZipEntry::file("", b"");
    cp437.name = vec![b'n', 0x82, b'e'];
    refused(&zip(&[cp437.clone()], false), "not flagged UTF-8");
    // UTF-8 without the flag is ambiguous too: CP437 tools read it otherwise.
    let mut unflagged = ZipEntry::file("", b"");
    unflagged.name = "na\u{ef}ve".as_bytes().to_vec();
    refused(&zip(&[unflagged.clone()], false), "not flagged UTF-8");
    unflagged.flags = 1 << 11;
    assert_eq!(nodes(&zip(&[unflagged], false))[0].0, "na\u{ef}ve");
    cp437.flags = 1 << 11;
    refused(&zip(&[cp437], false), "not flagged UTF-8");
}

#[test]
fn dot_git_is_refused_in_every_spelling() {
    // A git command run in the tree would read it: `core.fsmonitor` runs a command.
    for name in [
        ".git/config",
        ".GIT/config",
        "sub/.git",
        ".Git",
        ".g\u{200c}it/config",
        "\u{feff}.git",
        "GIT~1/config",
        ".git./x",
        ".git /x",
    ] {
        let tar = TarBuilder::default()
            .pax(b'x', &[("path", name)])
            .file("x", b"")
            .gz();
        let e = refusal(&tar);
        assert!(
            e.contains(".git") || e.contains("short (8.3)") || e.contains("dot or a space"),
            "{name:?}: {e}"
        );
    }
    // Names that only start like it are files like any other.
    for name in [".github/x", ".gitignore", "git", "x.git"] {
        let tar = TarBuilder::default()
            .pax(b'x', &[("path", name)])
            .file("x", b"")
            .gz();
        assert!(read(&tar, Limits::DEFAULT).is_ok(), "{name}");
    }
}

#[test]
fn a_repository_under_any_name_is_refused() {
    // Git takes a directory holding these for a repository, whatever its name, and a `git`
    // command run there runs its `config`'s `core.fsmonitor`.
    let repo = |dir: &str, names: &[&str]| {
        let mut tar = TarBuilder::default();
        for name in names {
            tar = match name.strip_suffix('/') {
                Some(d) => tar.dir(&format!("{dir}{d}")),
                None => tar.file(&format!("{dir}{name}"), b"x"),
            };
        }
        tar.file(
            &format!("{dir}config"),
            b"[core]\n\tfsmonitor = touch marker\n",
        )
        .gz()
    };
    for (dir, names) in [
        ("evil/", &["HEAD", "objects/", "refs/"][..]),
        ("", &["HEAD", "objects/", "refs/"]),
        ("a/b/", &["head", "OBJECTS/", "Refs/"]),
        ("evil/", &["HE\u{200c}AD", "objects/", "ref\u{17f}/"]),
        ("evil/", &["HEAD", "commondir"]),
        // Files named `objects` and `refs` too: refused, as git might take a link or a file
        // for them some day, and no package needs that layout.
        ("evil/", &["HEAD", "objects", "refs"]),
    ] {
        let e = refusal(&repo(dir, names));
        assert!(
            e.contains("take it for a repository"),
            "{dir}{names:?}: {e}"
        );
    }
    // Part of the layout is a tree like any other, and so is each part in another directory.
    for (dir, names) in [
        ("evil/", &["HEAD", "objects/"][..]),
        ("evil/", &["objects/", "refs/"]),
        ("evil/", &["commondir", "refs/"]),
    ] {
        assert!(
            read(&repo(dir, names), Limits::DEFAULT).is_ok(),
            "{names:?}"
        );
    }
    let apart = TarBuilder::default()
        .file("a/HEAD", b"x")
        .dir("b/objects")
        .dir("c/refs")
        .gz();
    assert!(read(&apart, Limits::DEFAULT).is_ok());
}

#[test]
fn short_names_with_tildes_long_names_and_clock_are_refused() {
    // A short name's stem may hold `~` itself.
    for name in ["A~B~CD~1", "A~B~CD~1.TXT", "x~~1"] {
        refused(&tar_with(name), "short (8.3) name");
    }
    for name in ["a~b", "readme~", "notes~old.txt"] {
        assert!(read(&tar_with(name), Limits::DEFAULT).is_ok(), "{name}");
    }
    for name in ["CLOCK$", "clock$.txt"] {
        refused(&tar_with(name), "a device name");
    }
    // A name past 255 bytes is refused when read, not when written.
    let long = |name: &str| {
        TarBuilder::default()
            .pax(b'x', &[("path", name)])
            .file("x", b"")
            .gz()
    };
    refused(&long(&"n".repeat(256)), "a name longer than 255 bytes");
    assert!(read(&long(&"n".repeat(255)), Limits::DEFAULT).is_ok());
    let pax = TarBuilder::default()
        .pax(b'x', &[("path", &format!("d/{}", "\u{e9}".repeat(128)))])
        .file("x", b"")
        .gz();
    refused(&pax, "a name longer than 255 bytes");
}

#[test]
fn names_that_differ_by_an_hfs_ignorable_code_point_clash() {
    // HFS+ takes `a\u{200c}b` for `ab`: two entries are one file there, and a link through one
    // spelling reaches the other.
    let tar = TarBuilder::default()
        .file("ab", b"1")
        .file("a\u{200c}b", b"2")
        .gz();
    refused(&tar, "");
    let e = refusal(&tar);
    assert!(
        e.contains("same") || e.contains("clash") || e.contains("one file"),
        "{e}"
    );
    let key = super::super::names::clash_key;
    assert_eq!(key("d\u{feff}ir/A\u{200d}B"), key("dir/ab"));
}
