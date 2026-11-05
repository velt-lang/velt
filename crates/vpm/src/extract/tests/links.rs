//! Symbolic and hard links: where they lead, through each other.

use super::*;

#[test]
fn links_inside_the_tree_are_kept() {
    let tar = TarBuilder::default()
        .file("lib/a.c", b"int a;")
        .symlink("include/a.c", "../lib/a.c")
        .symlink("here", ".")
        .symlink("lib/self", "../lib")
        .gz();
    assert_eq!(
        nodes(&tar),
        [
            ("here".to_string(), Node::Link(".".into())),
            ("include".to_string(), Node::Dir),
            ("include/a.c".to_string(), Node::Link("../lib/a.c".into())),
            ("lib".to_string(), Node::Dir),
            ("lib/a.c".to_string(), Node::File(b"int a;".to_vec())),
            ("lib/self".to_string(), Node::Link("../lib".into())),
        ]
    );
    let zip = zip(
        &[
            ZipEntry::symlink("l", "dir/x"),
            ZipEntry::file("dir/x", b""),
        ],
        false,
    );
    assert_eq!(
        nodes(&zip)[2],
        ("l".to_string(), Node::Link("dir/x".into()))
    );
}

#[test]
fn links_leading_outside_are_refused() {
    for (link, target, why) in [
        ("up", "..", "leads outside the archive"),
        ("a/b/up", "../../../etc", "leads outside the archive"),
        ("abs", "/etc/passwd", "a link to an absolute path"),
        ("win", "..\\x", "a Windows path or drive"),
        ("drive", "C:/x", "a Windows path or drive"),
        ("empty", "", "a link with an empty target"),
    ] {
        let tar = TarBuilder::default().symlink(link, target).gz();
        let e = refusal(&tar);
        assert!(
            e.contains(why),
            "{link} -> {target}: expected {why:?} in {e}"
        );
    }
    refused(
        &zip(&[ZipEntry::symlink("x", "../../x")], false),
        "leads outside the archive",
    );
}

#[test]
fn a_link_is_followed_through_the_archives_other_links() {
    // `d1/d2/l` leads to `d1`: fine. `m`'s text stays inside (`d1/d2/l/../..` is `d1` as text),
    // but through `l` it is `d1/../..`: outside.
    let tar = TarBuilder::default()
        .symlink("d1/d2/l", "..")
        .symlink("m", "d1/d2/l/../..")
        .gz();
    refused(
        &tar,
        "`m`: a link to `d1/d2/l/../..`, which leads outside the archive",
    );
    // The same chain, ending inside.
    let inside = TarBuilder::default()
        .symlink("d1/d2/l", "..")
        .symlink("m", "d1/d2/l/..")
        .gz();
    assert_eq!(nodes(&inside).len(), 4);
    // A loop.
    let wrapped = TarBuilder::default()
        .symlink("a", "b")
        .symlink("b", "a/x")
        .gz();
    refused(&wrapped, "more than 40 links");
}

#[test]
fn nothing_is_written_under_a_link() {
    // `escape/passwd` would be written through the link wherever it points.
    let tar = TarBuilder::default()
        .symlink("escape", "inside")
        .file("escape/passwd", b"x")
        .gz();
    refused(&tar, "`escape` is a link, so nothing can be under it");
    let after = TarBuilder::default()
        .file("escape/passwd", b"x")
        .symlink("escape", "inside")
        .gz();
    refused(&after, "is a directory and also a file or link");
    // Through a different case of the link's name: one directory on macOS.
    let case = TarBuilder::default()
        .symlink("Escape", "inside")
        .file("escape/passwd", b"x")
        .gz();
    refused(&case, "differ only by case");
}

#[test]
fn a_hard_link_is_a_copy_of_an_earlier_file() {
    let tar = TarBuilder::default()
        .file("a.txt", b"bytes")
        .hardlink("b.txt", "a.txt")
        .gz();
    assert_eq!(
        nodes(&tar)[1],
        ("b.txt".to_string(), Node::File(b"bytes".to_vec()))
    );
    for (target, why) in [
        ("missing", "not a file earlier in the archive"),
        ("../../etc/passwd", "its hard link target: a `..` segment"),
        ("/etc/passwd", "its hard link target: an absolute path"),
    ] {
        let tar = TarBuilder::default()
            .file("a.txt", b"x")
            .hardlink("b", target)
            .gz();
        let e = refusal(&tar);
        assert!(e.contains(why), "{target}: expected {why:?} in {e}");
    }
    // Only to a file: not to a directory or a link.
    let to_link = TarBuilder::default()
        .symlink("l", "x")
        .file("x", b"")
        .hardlink("h", "l")
        .gz();
    refused(&to_link, "not a file earlier in the archive");
    // Before its target, it is refused (tar's order).
    let early = TarBuilder::default().hardlink("h", "a").file("a", b"").gz();
    refused(&early, "not a file earlier in the archive");
}

#[test]
fn links_are_found_by_any_case_of_their_name() {
    // On macOS and Windows `d/UP` is the link `d/up`, so `escape` leads to the cache directory
    // (found in review: an exact-name lookup took `d/UP` for a plain directory).
    let tar = TarBuilder::default()
        .dir("d/")
        .symlink("d/up", "..")
        .symlink("escape", "d/UP/..")
        .gz();
    refused(
        &tar,
        "`escape`: a link to `d/UP/..`, which leads outside the archive",
    );
    // The other way round: the link is `D/Up`, reached as `d/up`.
    let upper = TarBuilder::default()
        .symlink("D/Up", "..")
        .symlink("escape", "D/up/..")
        .gz();
    refused(&upper, "leads outside the archive");
    // And by normalisation: `cafe\u{301}` is the link `caf\u{e9}`.
    let nfd = TarBuilder::default()
        .symlink("caf\u{e9}", "..")
        .symlink("x/escape", "../cafe\u{301}/x")
        .gz();
    refused(&nfd, "leads outside the archive");
}

#[test]
fn links_that_multiply_are_cut_short() {
    // `b` passes through `a` 2000 times, `c` through `b` 2000 times, `d` through `c`: following
    // them all would take billions of steps for a few kilobytes of archive.
    let many = |name: &str| vec![name; 2000].join("/");
    let tar = TarBuilder::default()
        .symlink("a", ".")
        .pax(b'x', &[("linkpath", &many("a"))])
        .symlink("b", "x")
        .pax(b'x', &[("linkpath", &many("b"))])
        .symlink("c", "x")
        .pax(b'x', &[("linkpath", &many("c"))])
        .symlink("d", "x")
        .gz();
    let started = std::time::Instant::now();
    refused(&tar, "takes too many steps to follow");
    assert!(started.elapsed().as_secs() < 30);
}
