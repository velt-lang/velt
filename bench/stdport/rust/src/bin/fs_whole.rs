// Whole files: the same program as bench/stdport/fs_whole.velt, on std::fs, as the runtime ran
// it (read_to_end, then the text checked as UTF-8; metadata; OpenOptions for write and append).
use std::fs::{self, OpenOptions};
use std::io::Write;

fn main() {
    let dir = std::env::temp_dir();
    let path = dir.join("velt_bench_fs_whole.txt");
    let small = dir.join("velt_bench_fs_whole_small.txt");
    fs::write(&path, "0123456789abcdéf".repeat(61681)).unwrap();
    let mut total: u64 = 0;
    for _ in 0..300 {
        // A JS string's length: UTF-16 units, as Velt's `length` counts.
        let text = String::from_utf8(fs::read(&path).unwrap()).unwrap();
        total += text.encode_utf16().count() as u64;
        total += fs::read(&path).unwrap().len() as u64;
    }
    for _ in 0..20000 {
        total += fs::metadata(&path).unwrap().len();
        total += u64::from(path.try_exists().unwrap_or(false));
    }
    for _ in 0..3000 {
        fs::write(&small, "one line of text\n").unwrap();
        let mut f = OpenOptions::new().create(true).append(true).open(&small).unwrap();
        f.write_all(b"and another\n").unwrap();
    }
    total += fs::read_to_string(&small).unwrap().len() as u64;
    println!("{total}");
}
