//! The system functions of macOS (libSystem) and Linux (glibc, musl), on arm64 and x86-64.
//! What differs between them: `struct flock` and `struct stat`, `fcntl`'s command numbers,
//! where `errno` lives and, on x86-64 macOS, `fstat`'s symbol.

use super::{After, Field, Func, Guard, Int, Param, Ret, Struct};

const fn int(name: &'static str, c: &'static str, ty: Int) -> Param {
    Param::Int {
        name,
        c,
        ty,
        guard: Guard::None,
    }
}

const FD: Param = int("fd", "int", Int::I32);
const INT: Ret = Ret::Int {
    c: "int",
    ty: Int::I32,
};
const SSIZE: Ret = Ret::Int {
    c: "ssize_t",
    ty: Int::ISize,
};

const fn field(name: &'static str, ty: Int, c: Int, offset: u32) -> Field {
    Field {
        name,
        ty,
        c,
        offset,
    }
}

/// `struct flock` on macOS: `off_t l_start, l_len; pid_t l_pid; short l_type, l_whence`.
static FLOCK_MACOS: Struct = Struct {
    c: "struct flock",
    size: 24,
    align: 8,
    fields: &[
        field("l_start", Int::I64, Int::I64, 0),
        field("l_len", Int::I64, Int::I64, 8),
        field("l_pid", Int::I32, Int::I32, 16),
        field("l_type", Int::I16, Int::I16, 20),
        field("l_whence", Int::I16, Int::I16, 22),
    ],
};

/// `struct flock` on Linux (both architectures): `short l_type, l_whence; off_t l_start, l_len;
/// pid_t l_pid`.
static FLOCK_LINUX: Struct = Struct {
    c: "struct flock",
    size: 32,
    align: 8,
    fields: &[
        field("l_type", Int::I16, Int::I16, 0),
        field("l_whence", Int::I16, Int::I16, 2),
        field("l_start", Int::I64, Int::I64, 8),
        field("l_len", Int::I64, Int::I64, 16),
        field("l_pid", Int::I32, Int::I32, 24),
    ],
};

/// `struct stat` of macOS (the 64-bit-inode one, `fstat$INODE64` on x86-64). The `st_*time`
/// fields are the seconds and nanoseconds of the `timespec`s.
static STAT_MACOS: Struct = Struct {
    c: "struct stat",
    size: 144,
    align: 8,
    fields: &[
        field("st_dev", Int::U64, Int::I32, 0),
        field("st_mode", Int::U32, Int::U16, 4),
        field("st_nlink", Int::U64, Int::U16, 6),
        field("st_ino", Int::U64, Int::U64, 8),
        field("st_uid", Int::U32, Int::U32, 16),
        field("st_gid", Int::U32, Int::U32, 20),
        field("st_rdev", Int::U64, Int::I32, 24),
        field("st_atime", Int::I64, Int::I64, 32),
        field("st_atime_nsec", Int::I64, Int::I64, 40),
        field("st_mtime", Int::I64, Int::I64, 48),
        field("st_mtime_nsec", Int::I64, Int::I64, 56),
        field("st_ctime", Int::I64, Int::I64, 64),
        field("st_ctime_nsec", Int::I64, Int::I64, 72),
        field("st_size", Int::I64, Int::I64, 96),
        field("st_blocks", Int::I64, Int::I64, 104),
        field("st_blksize", Int::I64, Int::I32, 112),
    ],
};

/// `struct stat` of x86-64 Linux.
static STAT_LINUX_X86: Struct = Struct {
    c: "struct stat",
    size: 144,
    align: 8,
    fields: &[
        field("st_dev", Int::U64, Int::U64, 0),
        field("st_ino", Int::U64, Int::U64, 8),
        field("st_nlink", Int::U64, Int::U64, 16),
        field("st_mode", Int::U32, Int::U32, 24),
        field("st_uid", Int::U32, Int::U32, 28),
        field("st_gid", Int::U32, Int::U32, 32),
        field("st_rdev", Int::U64, Int::U64, 40),
        field("st_size", Int::I64, Int::I64, 48),
        field("st_blksize", Int::I64, Int::I64, 56),
        field("st_blocks", Int::I64, Int::I64, 64),
        field("st_atime", Int::I64, Int::I64, 72),
        field("st_atime_nsec", Int::I64, Int::I64, 80),
        field("st_mtime", Int::I64, Int::I64, 88),
        field("st_mtime_nsec", Int::I64, Int::I64, 96),
        field("st_ctime", Int::I64, Int::I64, 104),
        field("st_ctime_nsec", Int::I64, Int::I64, 112),
    ],
};

/// `struct stat` of arm64 Linux (the generic layout).
static STAT_LINUX_ARM: Struct = Struct {
    c: "struct stat",
    size: 128,
    align: 8,
    fields: &[
        field("st_dev", Int::U64, Int::U64, 0),
        field("st_ino", Int::U64, Int::U64, 8),
        field("st_mode", Int::U32, Int::U32, 16),
        field("st_nlink", Int::U64, Int::U32, 20),
        field("st_uid", Int::U32, Int::U32, 24),
        field("st_gid", Int::U32, Int::U32, 28),
        field("st_rdev", Int::U64, Int::U64, 32),
        field("st_size", Int::I64, Int::I64, 48),
        field("st_blksize", Int::I64, Int::I32, 56),
        field("st_blocks", Int::I64, Int::I64, 64),
        field("st_atime", Int::I64, Int::I64, 72),
        field("st_atime_nsec", Int::I64, Int::I64, 80),
        field("st_mtime", Int::I64, Int::I64, 88),
        field("st_mtime_nsec", Int::I64, Int::I64, 96),
        field("st_ctime", Int::I64, Int::I64, 104),
        field("st_ctime_nsec", Int::I64, Int::I64, 112),
    ],
};

/// `fcntl` commands whose argument is an `int` (or nothing): `F_GETFD`, `F_SETFD`, `F_GETFL`,
/// `F_SETFL`, and on macOS `F_NOCACHE`, `F_FULLFSYNC`, `F_BARRIERFSYNC`.
const FCNTL_INT_MACOS: &[i64] = &[1, 2, 3, 4, 48, 51, 85];
const FCNTL_INT_LINUX: &[i64] = &[1, 2, 3, 4];
/// `fcntl` commands whose argument is a `struct flock *`: `F_GETLK`, `F_SETLK`, `F_SETLKW`, and
/// on Linux the open-file-description locks `F_OFD_GETLK`, `F_OFD_SETLK`, `F_OFD_SETLKW`.
const FCNTL_LOCK_MACOS: &[i64] = &[7, 8, 9];
const FCNTL_LOCK_LINUX: &[i64] = &[5, 6, 7, 36, 37, 38];

/// One POSIX table: what differs is passed in.
macro_rules! posix_table {
    ($name:ident, $flock:expr, $stat:expr, $fstat:expr, $errno:expr, $fint:expr, $flk:expr) => {
        static $name: &[Func] = &[
            Func {
                name: "open",
                symbol: "open",
                params: &[
                    Param::CStr { name: "path" },
                    int("oflag", "int", Int::I32),
                    int("mode", "mode_t", Int::U32),
                ],
                variadic: 2,
                ret: INT,
                after: After::None,
            },
            simple("close", &[FD], INT),
            simple(
                "read",
                &[FD, Param::Buf { name: "buf", written: true, len: Int::USize }],
                SSIZE,
            ),
            simple(
                "write",
                &[FD, Param::Buf { name: "buf", written: false, len: Int::USize }],
                SSIZE,
            ),
            simple(
                "pread",
                &[
                    FD,
                    Param::Buf { name: "buf", written: true, len: Int::USize },
                    int("offset", "off_t", Int::I64),
                ],
                SSIZE,
            ),
            simple(
                "pwrite",
                &[
                    FD,
                    Param::Buf { name: "buf", written: false, len: Int::USize },
                    int("offset", "off_t", Int::I64),
                ],
                SSIZE,
            ),
            simple("fsync", &[FD], INT),
            simple("fdatasync", &[FD], INT),
            simple("ftruncate", &[FD, int("length", "off_t", Int::I64)], INT),
            simple(
                "lseek",
                &[FD, int("offset", "off_t", Int::I64), int("whence", "int", Int::I32)],
                Ret::Int { c: "off_t", ty: Int::I64 },
            ),
            simple("unlink", &[Param::CStr { name: "path" }], INT),
            Func {
                name: "fcntl",
                symbol: "fcntl",
                params: &[
                    FD,
                    Param::Int { name: "cmd", c: "int", ty: Int::I32, guard: Guard::OneOf($fint) },
                    int("arg", "int", Int::I32),
                ],
                variadic: 2,
                ret: INT,
                after: After::None,
            },
            Func {
                name: "fcntl",
                symbol: "fcntl",
                params: &[
                    FD,
                    Param::Int { name: "cmd", c: "int", ty: Int::I32, guard: Guard::OneOf($flk) },
                    Param::Struct { name: "lock", layout: $flock },
                ],
                variadic: 2,
                ret: INT,
                after: After::None,
            },
            Func {
                name: "fstat",
                symbol: $fstat,
                params: &[FD, Param::Struct { name: "st", layout: $stat }],
                variadic: 2,
                ret: INT,
                after: After::None,
            },
            Func {
                name: "lastOsError",
                symbol: $errno,
                params: &[],
                variadic: 0,
                ret: Ret::Errno,
                after: After::None,
            },
        ];
    };
}

const fn simple(name: &'static str, params: &'static [Param], ret: Ret) -> Func {
    Func {
        name,
        symbol: name,
        params,
        variadic: params.len(),
        ret,
        after: After::None,
    }
}

posix_table!(MACOS_ARM, &FLOCK_MACOS, &STAT_MACOS, "fstat", "__error", FCNTL_INT_MACOS, FCNTL_LOCK_MACOS);
posix_table!(
    MACOS_X86,
    &FLOCK_MACOS,
    &STAT_MACOS,
    "fstat$INODE64",
    "__error",
    FCNTL_INT_MACOS,
    FCNTL_LOCK_MACOS
);
posix_table!(
    LINUX_ARM,
    &FLOCK_LINUX,
    &STAT_LINUX_ARM,
    "fstat",
    "__errno_location",
    FCNTL_INT_LINUX,
    FCNTL_LOCK_LINUX
);
posix_table!(
    LINUX_X86,
    &FLOCK_LINUX,
    &STAT_LINUX_X86,
    "fstat",
    "__errno_location",
    FCNTL_INT_LINUX,
    FCNTL_LOCK_LINUX
);

pub(super) fn macos(arm: bool) -> &'static [Func] {
    if arm {
        MACOS_ARM
    } else {
        MACOS_X86
    }
}

pub(super) fn linux(arm: bool) -> &'static [Func] {
    if arm {
        LINUX_ARM
    } else {
        LINUX_X86
    }
}
