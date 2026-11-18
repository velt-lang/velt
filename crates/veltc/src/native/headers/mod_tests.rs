use super::*;

#[test]
fn clang_s_command_line_splits_into_words() {
    assert_eq!(
        split_quoted(r#" "/z/zig" "-cc1" "-I/a b" "-DX=\"y\"""#),
        ["/z/zig", "-cc1", "-I/a b", "-DX=\"y\""]
    );
}

#[test]
fn the_include_directory_reaches_clang_without_the_verbatim_prefix() {
    assert_eq!(include_arg(Path::new(r"\\?\C:\x")), r"-IC:\x");
    assert_eq!(
        include_arg(Path::new(r"\\?\UNC\server\share")),
        r"-I\\server\share"
    );
    assert_eq!(include_arg(Path::new("/pkg/sqlite")), "-I/pkg/sqlite");
    assert_eq!(include_arg(Path::new(r"E:\velt\x")), r"-IE:\velt\x");
}

#[test]
fn clang_s_dependency_list_gives_the_files_read() {
    // A file outside the parse's directory, as an absolute path of this host (a drive
    // letter on Windows), with spaces escaped as clang does.
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().parent().unwrap().join("a.h");
    let listed = outside.to_str().unwrap().replace(' ', "\\ ");
    assert_eq!(
        files_read(dir.path(), &format!("deps: headers.c {listed}\n")),
        [outside]
    );
}

/// A remembered check holds while every file it read is the same; an older entry, without
/// the files, doesn't.
#[test]
fn a_remembered_check_holds_while_its_files_do() {
    let dir = tempfile::tempdir().unwrap();
    let (outer, inner) = (dir.path().join("t.h"), dir.path().join("inner.h"));
    std::fs::write(&outer, "#include \"inner.h\"\n").unwrap();
    std::fs::write(&inner, "int f(int a);\n").unwrap();
    let remembered = dir.path().join("remembered");
    assert!(still_holds(&remembered).is_none());
    let read = [outer, inner.clone()].map(|f| {
        let digest = file_digest(&f).unwrap();
        (f, digest)
    });
    std::fs::write(&remembered, remembered_text(&read).unwrap()).unwrap();
    assert_eq!(still_holds(&remembered).unwrap(), read);
    std::fs::write(&inner, "int f(long a);\n").unwrap();
    assert!(still_holds(&remembered).is_none());
    std::fs::remove_file(&inner).unwrap();
    assert!(still_holds(&remembered).is_none());
    std::fs::write(&remembered, "checked\n").unwrap();
    assert!(still_holds(&remembered).is_none());
    std::fs::write(&remembered, "").unwrap();
    assert!(still_holds(&remembered).is_none());
}

/// Remembering replaces the file rather than writing into it: what another name of the old
/// file reads stays whole (as a reader in the middle of the write would see it).
#[test]
fn remembering_replaces_the_file_whole() {
    let dir = tempfile::tempdir().unwrap();
    let header = dir.path().join("a.h");
    std::fs::write(&header, "int f(int a);\n").unwrap();
    let remembered = dir.path().join("remembered");
    std::fs::write(&remembered, "old\n").unwrap();
    let other_name = dir.path().join("other");
    std::fs::hard_link(&remembered, &other_name).unwrap();
    let read = [(header.clone(), file_digest(&header).unwrap())];
    remember(&remembered, &read);
    assert_eq!(std::fs::read_to_string(&other_name).unwrap(), "old\n");
    assert_eq!(still_holds(&remembered).unwrap(), read);
}

/// What the stamp gets: the package headers each check read, by digest; not one the
/// package doesn't have.
#[test]
fn the_stamp_has_the_package_files_a_check_read() {
    let dir = tempfile::tempdir().unwrap();
    let header = dir.path().join("native/own.h");
    std::fs::create_dir(dir.path().join("native")).unwrap();
    std::fs::write(&header, "int f(int a);\n").unwrap();
    let mut checked = Checked::new();
    let record = LibraryRecord {
        declarations: Default::default(),
        headers: BTreeMap::from([
            ("native/own.h".to_string(), String::new()),
            ("native/gone.h".to_string(), String::new()),
        ]),
        defines: String::new(),
    };
    checked
        .entry(dir.path().to_path_buf())
        .or_default()
        .insert("own".into(), record);
    let mut inputs = Vec::new();
    stamp_inputs(&checked, &mut inputs);
    let hash = vpm::probe::content_hash(b"int f(int a);\n");
    let path = vpm::relpath::absolute(&header);
    assert_eq!(inputs, [format!("file {hash}\t{}", path.display())]);
}

#[test]
fn each_parse_has_its_own_directory() {
    let home = Path::new("/h");
    assert_ne!(work_dir(home), work_dir(home));
}

#[test]
fn a_package_s_headers_are_included_by_their_path_in_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("native")).unwrap();
    std::fs::write(dir.path().join("native/a.h"), "").unwrap();
    let headers = ["native/a.h".to_string(), "<string.h>".to_string()];
    assert_eq!(
        includes(dir.path(), &headers).unwrap(),
        "#include \"native/a.h\"\n#include <string.h>\n"
    );
    let missing = includes(dir.path(), &["native/b.h".to_string()]).unwrap_err();
    assert_eq!(missing, "header `native/b.h` is not in the package");
}

#[test]
fn targets_map_to_zig_s() {
    assert_eq!(
        zig_target("x86_64-pc-windows-msvc"),
        Some("x86_64-windows-gnu")
    );
    assert_eq!(zig_target("aarch64-apple-darwin"), Some("aarch64-macos"));
    assert_eq!(zig_target("wasm32-wasip1"), None);
}
