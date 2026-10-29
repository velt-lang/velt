//! The system functions of Windows (kernel32), x86-64 and arm64. A `HANDLE` is a `usize`.
//!
//! The functions that wait for an `OVERLAPPED` (`ReadFile`, `WriteFile`, `LockFileEx`,
//! `UnlockFileEx`) are checked but not callable yet ([`super::callable`]); the others are, and
//! the `lang/system_function_windows` golden runs them.

use super::{After, Field, Func, Guard, Int, Param, Ret, Struct};

const fn int(name: &'static str, c: &'static str, ty: Int) -> Param {
    Param::Int {
        name,
        c,
        ty,
        guard: Guard::None,
    }
}

const HANDLE: Param = int("file", "HANDLE", Int::USize);
const BOOL: Ret = Ret::Int {
    c: "BOOL",
    ty: Int::I32,
};

/// `OVERLAPPED`: `ULONG_PTR Internal, InternalHigh; DWORD Offset, OffsetHigh; HANDLE hEvent`.
/// `hEvent` is not a field Velt can set: it stays 0.
///
/// SAFETY, before the functions taking one become callable ([`super::callable`]): a handle
/// associated with an I/O completion port queues a completion packet for every overlapped
/// operation unless `hEvent`'s low bit is set (`GetQueuedCompletionStatus` remarks), and the
/// packet's `lpOverlapped` would point at the temporary lowering built, gone by the time the
/// port is drained. Lowering must set `hEvent` to an event handle with its low bit set (or the
/// call must reject a handle bound to a port) before they are enabled.
static OVERLAPPED: Struct = Struct {
    c: "OVERLAPPED",
    size: 32,
    align: 8,
    fields: &[
        Field {
            name: "Internal",
            ty: Int::USize,
            c: Int::USize,
            offset: 0,
        },
        Field {
            name: "InternalHigh",
            ty: Int::USize,
            c: Int::USize,
            offset: 8,
        },
        Field {
            name: "Offset",
            ty: Int::U32,
            c: Int::U32,
            offset: 16,
        },
        Field {
            name: "OffsetHigh",
            ty: Int::U32,
            c: Int::U32,
            offset: 20,
        },
    ],
};

/// `LARGE_INTEGER`.
static LARGE_INTEGER: Struct = Struct {
    c: "LARGE_INTEGER",
    size: 8,
    align: 8,
    fields: &[Field {
        name: "QuadPart",
        ty: Int::I64,
        c: Int::I64,
        offset: 0,
    }],
};

const OV: Param = Param::Struct {
    name: "lpOverlapped",
    layout: &OVERLAPPED,
};

/// `FILE_FLAG_OVERLAPPED`: reads and writes would finish after the call returned.
const FILE_FLAG_OVERLAPPED: i64 = 0x4000_0000;

const fn func(name: &'static str, params: &'static [Param], ret: Ret, after: After) -> Func {
    Func {
        name,
        symbol: name,
        params,
        variadic: params.len(),
        ret,
        after,
    }
}

/// `ReadFile`, `WriteFile`, `LockFileEx`, `UnlockFileEx`: wait for an operation that is still
/// pending (a handle opened for overlapped I/O elsewhere).
const fn wait(overlapped: usize) -> After {
    After::WaitOverlapped {
        handle: 0,
        overlapped,
    }
}

static TABLE: &[Func] = &[
    func(
        "CreateFileW",
        &[
            Param::WStr { name: "lpFileName" },
            int("dwDesiredAccess", "DWORD", Int::U32),
            int("dwShareMode", "DWORD", Int::U32),
            Param::Null {
                name: "lpSecurityAttributes",
                c: "LPSECURITY_ATTRIBUTES",
            },
            int("dwCreationDisposition", "DWORD", Int::U32),
            Param::Int {
                name: "dwFlagsAndAttributes",
                c: "DWORD",
                ty: Int::U32,
                guard: Guard::NoBits(FILE_FLAG_OVERLAPPED),
            },
            Param::Null {
                name: "hTemplateFile",
                c: "HANDLE",
            },
        ],
        Ret::Int {
            c: "HANDLE",
            ty: Int::USize,
        },
        After::None,
    ),
    func(
        "ReadFile",
        &[
            HANDLE,
            Param::Buf {
                name: "lpBuffer",
                written: true,
                len: Int::U32,
            },
            Param::Null {
                name: "lpNumberOfBytesRead",
                c: "LPDWORD",
            },
            OV,
        ],
        BOOL,
        wait(3),
    ),
    func(
        "WriteFile",
        &[
            HANDLE,
            Param::Buf {
                name: "lpBuffer",
                written: false,
                len: Int::U32,
            },
            Param::Null {
                name: "lpNumberOfBytesWritten",
                c: "LPDWORD",
            },
            OV,
        ],
        BOOL,
        wait(3),
    ),
    func("FlushFileBuffers", &[HANDLE], BOOL, After::None),
    func("SetEndOfFile", &[HANDLE], BOOL, After::None),
    func(
        "SetFilePointerEx",
        &[
            HANDLE,
            int("liDistanceToMove", "LARGE_INTEGER", Int::I64),
            Param::Null {
                name: "lpNewFilePointer",
                c: "PLARGE_INTEGER",
            },
            int("dwMoveMethod", "DWORD", Int::U32),
        ],
        BOOL,
        After::None,
    ),
    func(
        "GetFileSizeEx",
        &[
            HANDLE,
            Param::Struct {
                name: "lpFileSize",
                layout: &LARGE_INTEGER,
            },
        ],
        BOOL,
        After::None,
    ),
    func(
        "LockFileEx",
        &[
            HANDLE,
            int("dwFlags", "DWORD", Int::U32),
            int("dwReserved", "DWORD", Int::U32),
            int("nNumberOfBytesToLockLow", "DWORD", Int::U32),
            int("nNumberOfBytesToLockHigh", "DWORD", Int::U32),
            OV,
        ],
        BOOL,
        wait(5),
    ),
    func(
        "UnlockFileEx",
        &[
            HANDLE,
            int("dwReserved", "DWORD", Int::U32),
            int("nNumberOfBytesToUnlockLow", "DWORD", Int::U32),
            int("nNumberOfBytesToUnlockHigh", "DWORD", Int::U32),
            OV,
        ],
        BOOL,
        wait(4),
    ),
    func(
        "CloseHandle",
        &[int("hObject", "HANDLE", Int::USize)],
        BOOL,
        After::None,
    ),
    Func {
        name: "lastOsError",
        symbol: "GetLastError",
        params: &[],
        variadic: 0,
        // A `DWORD` in the same register: the declaration reads it as an `i32`.
        ret: Ret::Int {
            c: "DWORD",
            ty: Int::I32,
        },
        after: After::None,
    },
];

pub(super) fn table() -> &'static [Func] {
    TABLE
}
