use super::*;

fn size_of(i: Int) -> u32 {
    match i {
        Int::I16 | Int::U16 => 2,
        Int::I32 | Int::U32 => 4,
        Int::I64 | Int::U64 | Int::ISize | Int::USize => 8,
    }
}

/// Every C struct's fields lie inside it, aligned, without overlapping, and its size is a
/// multiple of its alignment.
#[test]
fn struct_layouts_are_consistent() {
    for t in TARGETS {
        for f in table(t) {
            for p in f.params {
                let Param::Struct { layout, .. } = p else {
                    continue;
                };
                assert_eq!(layout.size % layout.align, 0, "{t} {}", layout.c);
                let mut spans: Vec<(u32, u32)> = vec![];
                for fd in layout.fields {
                    let n = size_of(fd.c);
                    assert_eq!(fd.offset % n, 0, "{t} {}.{} unaligned", layout.c, fd.name);
                    assert!(fd.offset + n <= layout.size, "{t} {}.{}", layout.c, fd.name);
                    assert!(
                        size_of(fd.ty) >= n,
                        "{t} {}.{}: the Velt type is narrower than C's",
                        layout.c,
                        fd.name
                    );
                    for &(o, m) in &spans {
                        assert!(fd.offset >= o + m || fd.offset + n <= o, "{t} {}", fd.name);
                    }
                    spans.push((fd.offset, n));
                }
            }
        }
    }
}

/// Two forms of one function link one symbol with one signature (variadic arguments are all
/// 64-bit), and only variadic functions have several forms.
#[test]
fn overloads_share_their_symbol() {
    for t in TARGETS {
        for f in table(t) {
            for g in table(t).iter().filter(|g| g.name == f.name) {
                assert_eq!(f.symbol, g.symbol, "{t} {}", f.name);
                assert_eq!(f.variadic, g.variadic, "{t} {}", f.name);
                assert_eq!(f.params.len(), g.params.len(), "{t} {}", f.name);
                if !std::ptr::eq(f, g) {
                    assert!(f.variadic < f.params.len(), "{t} {}", f.name);
                }
            }
        }
    }
}

/// Every target with a table has `lastOsError`, and the POSIX ones the same functions.
#[test]
fn every_table_reads_the_error() {
    for t in TARGETS {
        assert!(available(t, "lastOsError"), "{t}");
    }
    let names = |t: &str| table(t).iter().map(|f| f.name).collect::<Vec<_>>();
    assert_eq!(
        names("aarch64-apple-darwin"),
        names("x86_64-unknown-linux-gnu")
    );
    assert!(table("wasm32-wasip1").is_empty());
    assert!(table("wasm32-unknown-unknown").is_empty());
    assert_eq!(base_name("fcntl$lock"), "fcntl");
    assert_eq!(
        target_os("aarch64-unknown-linux-musl"),
        Some((Os::Linux, true))
    );
}

#[test]
fn prototypes_read_like_c() {
    let t = "aarch64-apple-darwin";
    let pread = overloads(t, "pread").next().unwrap();
    assert_eq!(
        pread.prototype(),
        "ssize_t pread(int fd, void *buf, size_t len, off_t offset)"
    );
    let open = overloads(t, "open").next().unwrap();
    assert_eq!(
        open.prototype(),
        "int open(const char *path, int oflag, ..., mode_t mode)"
    );
    assert_eq!(
        expected_decls(t, "pwrite"),
        ["declare system function pwrite(fd: i32, buf: readonly u8[], offset: i64): isize;"]
    );
    assert_eq!(
        expected_decls(t, "fcntl")[1],
        "declare system function fcntl(fd: i32, cmd: i32, lock: Flock): i32;"
    );
}
