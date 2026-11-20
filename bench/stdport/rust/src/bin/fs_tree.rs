// Directories: the same program as bench/stdport/fs_tree.velt, on std::fs, as the runtime ran it
// (read_dir with the names sorted, an entry's file_type, create_dir_all, remove_dir_all).
use std::fs;
use std::path::Path;

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort_unstable();
    names
}

/// The sorted names, each with whether it is a file and whether it is a directory.
fn entries(dir: &Path) -> Vec<(String, bool, bool)> {
    let mut entries: Vec<(String, bool, bool)> = fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let t = e.file_type().unwrap();
            (e.file_name().into_string().unwrap(), t.is_file(), t.is_dir())
        })
        .collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    entries
}

fn main() {
    let dir = std::env::temp_dir().join("velt_bench_fs_tree");
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    let many = dir.join("many");
    fs::create_dir_all(&many).unwrap();
    for i in 0..1000 {
        fs::write(many.join(format!("file-{}.txt", (i * 7919) % 1000)), "").unwrap();
    }
    fs::create_dir(many.join("sub")).unwrap();
    let mut total: u64 = 0;
    for i in 0..300 {
        let names = names(&many);
        total += (names.len() + names[i].len()) as u64;
        let entries = entries(&many);
        total += if entries[i].1 { 2 } else { 1 };
        total += if entries[entries.len() - 1].2 { 5 } else { 3 };
    }
    let tree = dir.join("t");
    for _ in 0..300 {
        fs::create_dir_all(tree.join("a/b/c/d")).unwrap();
        fs::create_dir_all(tree.join("a/x/y")).unwrap();
        fs::write(tree.join("f0"), "x").unwrap();
        fs::write(tree.join("a/f1"), "x").unwrap();
        fs::write(tree.join("a/b/f2"), "x").unwrap();
        fs::write(tree.join("a/b/c/f3"), "x").unwrap();
        fs::write(tree.join("a/b/c/d/f4"), "x").unwrap();
        fs::write(tree.join("a/x/y/f5"), "x").unwrap();
        total += names(&tree.join("a")).len() as u64;
        fs::remove_dir_all(&tree).unwrap();
    }
    fs::remove_dir_all(&dir).unwrap();
    println!("{total}");
}
