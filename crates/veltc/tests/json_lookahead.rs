//! The decoder generated for a union told apart by a discriminant skips the fields before the
//! discriminant with the reader's `skipLookahead` (std/encoding/json/reader.vlt), which remembers where
//! skipped objects end, so nested unions decode in linear time. A plain skip here would rescan
//! every subtree once per enclosing level.

mod no_window;
mod test_dir;

#[test]
fn union_lookahead_uses_the_remembering_skip() {
    let dir = test_dir::TestDir::new();
    let src = dir.path().join("main.vlt");
    std::fs::write(
        &src,
        "class Leaf { v: i64 = 0; kind: \"leaf\" = \"leaf\"; }\n\
         class Node { child: Leaf | Node = new Leaf(); kind: \"node\" = \"node\"; }\n\
         function main() {\n\
           const t = JSON.parse<Leaf | Node>('{\"child\":{\"v\":1,\"kind\":\"leaf\"},\"kind\":\"node\"}');\n\
           console.log(t instanceof Node);\n\
         }\n",
    )
    .unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--emit", "vir"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let vir = String::from_utf8_lossy(&out.stdout);
    assert!(
        vir.contains("ReaderM13skipLookahead("),
        "the union decoder does not use the reader's skipLookahead"
    );
}
