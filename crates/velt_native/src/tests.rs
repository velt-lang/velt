//! The SDK against a mock function table: argument conversion, results, panics and the signature
//! records `velt native build` reads.

use super::*;
use std::borrow::Cow;
use std::sync::Once;

// Mock strings: words[0] = pointer to a leaked `Vec<u8>` (a builder's spare room past the text),
// words[1] = length of the text, words[2] = 1 when there is a vector.
unsafe extern "C" fn str_new(p: *const u8, len: usize, out: *mut VeltStr) {
    let v = Box::new(std::slice::from_raw_parts(p, len).to_vec());
    out.write(VeltStr {
        words: [Box::into_raw(v) as u64, len as u64, 1],
    });
}
unsafe extern "C" fn str_bytes(s: *const VeltStr, len: *mut usize) -> *const u8 {
    let w = (*s).words;
    if w[2] == 0 {
        len.write(0);
        return std::ptr::null();
    }
    let v = &*(w[0] as *const Vec<u8>);
    len.write(w[1] as usize);
    v.as_ptr()
}
unsafe extern "C" fn str_retain(s: *const VeltStr, out: *mut VeltStr) {
    let mut len = 0;
    let p = str_bytes(s, &mut len);
    str_new(if len == 0 { [].as_ptr() } else { p }, len, out);
}
unsafe extern "C" fn strbuf_spare(s: *mut VeltStr, min: u64, avail: *mut u64) -> *mut u8 {
    if (*s).words[2] == 0 {
        str_new([].as_ptr(), 0, s);
    }
    let w = (*s).words;
    let v = &mut *(w[0] as *mut Vec<u8>);
    let need = w[1] as usize + min as usize;
    if v.len() < need {
        v.resize(need, 0);
    }
    avail.write((v.len() - w[1] as usize) as u64);
    v.as_mut_ptr().add(w[1] as usize)
}
unsafe extern "C" fn strbuf_commit(s: *mut VeltStr, n: u64, _ascii: u8) {
    (*s).words[1] += n;
}
unsafe extern "C" fn str_utf8(s: *const VeltStr, len: *mut usize) -> *const u8 {
    let p = str_bytes(s, len);
    let bytes = if *len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(p, *len)
    };
    if std::str::from_utf8(bytes).is_ok() {
        p
    } else {
        std::ptr::null()
    }
}
unsafe extern "C" fn str_drop(s: *mut VeltStr) {
    if (*s).words[2] != 0 {
        drop(Box::from_raw((*s).words[0] as *mut Vec<u8>));
    }
    (*s).words = [0; 3];
}
// Mock byte arrays: words[0] = pointer to a leaked `Vec<u8>` (0: empty).
unsafe extern "C" fn bytes_new(p: *const u8, len: usize, out: *mut VeltBytes) {
    let v = Box::new(std::slice::from_raw_parts(p, len).to_vec());
    out.write(VeltBytes {
        words: [Box::into_raw(v) as u64, 0, 0],
    });
}
unsafe extern "C" fn bytes_drop(b: *mut VeltBytes) {
    if (*b).words[0] != 0 {
        drop(Box::from_raw((*b).words[0] as *mut Vec<u8>));
    }
    (*b).words = [0; 3];
}
unsafe extern "C" fn bytes_data(b: *const VeltBytes, len: *mut usize) -> *const u8 {
    if (*b).words[0] == 0 {
        len.write(0);
        return std::ptr::null();
    }
    let v = &*((*b).words[0] as *const Vec<u8>);
    len.write(v.len());
    v.as_ptr()
}
fn mock_bytes(b: &[u8]) -> Array<u8> {
    Array::from(b.to_vec())
}
fn rust_bytes(b: &Array<u8>) -> Vec<u8> {
    b.as_slice().to_vec()
}
/// A `u8[]` argument (an `Array<u8>` has the argument's layout).
fn arg(b: &Array<u8>) -> *const crate::value::RawArray<u8> {
    b as *const Array<u8> as *const crate::value::RawArray<u8>
}
// Mock blocking futures run at once; the "future" is a leaked 16-aligned buffer whose result
// slot is at +16, like the runtime's.
unsafe extern "C" fn fut_blocking(
    work: WorkFn,
    ctx: *mut c_void,
    _drop_ctx: DropCtxFn,
    size: usize,
    _drop_result: Option<DropResultFn>,
) -> *mut c_void {
    let buf = Box::into_raw(vec![0u128; 1 + size.div_ceil(16)].into_boxed_slice()) as *mut u8;
    work(ctx, buf.add(16));
    buf as *mut c_void
}
unsafe extern "C" fn fut_completer(_: usize, _: Option<DropResultFn>, h: *mut u64) -> *mut c_void {
    h.write(7);
    std::ptr::null_mut()
}
// The error code and message of the last `IoStatus` completed through the mock.
static COMPLETED: std::sync::Mutex<Option<(u64, i32, String)>> = std::sync::Mutex::new(None);
unsafe extern "C" fn complete(h: u64, result: *const u8) {
    let e = &*(result as *const VeltErr);
    let msg = str_of(&e.message).to_string();
    *COMPLETED.lock().unwrap() = Some((h, e.code, msg));
}
unsafe extern "C" fn fatal(msg: *const u8, len: usize) -> ! {
    panic!(
        "fatal: {}",
        String::from_utf8_lossy(std::slice::from_raw_parts(msg, len))
    )
}

// The async entries are exercised against the real runtime (velt_rt's `native/aio_tests.rs`).
macro_rules! unused {
    ($name:ident ( $($a:ty),* $(,)? ) $(-> $r:ty)?) => {
        unsafe extern "C" fn $name($(_: $a),*) $(-> $r)? {
            panic!(concat!("the mock table has no ", stringify!($name)))
        }
    };
}
unused!(fut_poll(crate::rt::PollFn, *mut c_void, crate::DropCtxFn, usize) -> *mut c_void);
unused!(cx_waker(*mut c_void) -> *const c_void);
unused!(waker_clone(*const c_void) -> *mut c_void);
unused!(waker_wake(*mut c_void));
unused!(waker_wake_by_ref(*const c_void));
unused!(waker_drop(*mut c_void));
unused!(
    fut_poll_in(
        crate::rt::PollFn,
        crate::DropCtxFn,
        usize,
        usize,
        usize,
        *mut *mut c_void,
    ) -> *mut c_void
);
unused!(fut_poll_ext(*mut c_void, *const crate::rt::WakerArg) -> u32);
unused!(fut_drop(*mut c_void));
unused!(spawn(*mut c_void));
unused!(tcp_connect(*const u8, usize) -> *mut c_void);
unused!(stream_from_socket(i64, *mut u64) -> i32);
unused!(
    stream_poll_read(u64, *const crate::rt::WakerArg, *mut u8, usize, *mut usize, *mut i32) -> u32
);
unused!(
    stream_poll_write(
        u64,
        *const crate::rt::WakerArg,
        *const u8,
        usize,
        *mut usize,
        *mut i32,
    ) -> u32
);
unused!(stream_poll_flush(u64, *const crate::rt::WakerArg, *mut i32) -> u32);
unused!(stream_poll_shutdown(u64, *const crate::rt::WakerArg, *mut i32) -> u32);
unused!(stream_socket(u64) -> i64);
unused!(stream_close(u64));
unused!(sleep_poll(*mut c_void, *const crate::rt::WakerArg) -> u32);
unused!(sleep_reset(*mut c_void, u64));
unused!(sleep_drop(*mut c_void));
extern "C" fn now_ns() -> u64 {
    0
}
extern "C" fn sleep_new(_: u64) -> *mut c_void {
    panic!("the mock table has no sleep_new")
}
extern "C" fn worker_index() -> i64 {
    -1
}
extern "C" fn alloc(size: u64, align: u64) -> *mut u8 {
    // SAFETY: tests allocate non-zero sizes with valid alignments.
    unsafe {
        std::alloc::alloc(
            std::alloc::Layout::from_size_align(size as usize, align as usize).unwrap(),
        )
    }
}
unsafe extern "C" fn realloc(p: *mut u8, old: u64, align: u64, new: u64) -> *mut u8 {
    std::alloc::realloc(
        p,
        std::alloc::Layout::from_size_align(old as usize, align as usize).unwrap(),
        new as usize,
    )
}
unsafe extern "C" fn free(p: *mut u8, size: u64, align: u64) {
    std::alloc::dealloc(
        p,
        std::alloc::Layout::from_size_align(size as usize, align as usize).unwrap(),
    )
}

static MOCK: Api = Api {
    abi_version: crate::ABI_VERSION,
    size: std::mem::size_of::<Api>() as u32,
    str_new,
    str_bytes,
    str_drop,
    bytes_new,
    bytes_drop,
    bytes_data,
    fut_blocking,
    fut_completer,
    complete,
    fatal,
    fut_poll,
    cx_waker,
    waker_clone,
    waker_wake,
    waker_wake_by_ref,
    waker_drop,
    fut_poll_ext,
    fut_drop,
    spawn,
    tcp_connect,
    stream_from_socket,
    stream_poll_read,
    stream_poll_write,
    stream_poll_flush,
    stream_poll_shutdown,
    stream_socket,
    stream_close,
    now_ns,
    sleep_new,
    sleep_poll,
    sleep_reset,
    sleep_drop,
    worker_index,
    alloc,
    realloc,
    free,
    str_from_utf8: str_new,
    str_retain,
    strbuf_spare,
    strbuf_commit,
    str_utf8,
    fut_poll_in,
};

crate::package!(demo);

fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| assert_eq!(unsafe { __velt_native_init(&MOCK) }, 0));
}

fn velt_str(s: &str) -> VeltStr {
    new_str(s)
}

fn rust_str(s: &VeltStr) -> String {
    unsafe { str_of(s) }.to_string()
}

#[export]
fn demo_add(a: u64, b: i32) -> u64 {
    a + b as u64
}

#[export]
fn demo_greet(name: &str, excited: bool) -> String {
    format!("hello, {name}{}", if excited { "!" } else { "" })
}

#[export]
fn demo_borrowed(text: Cow<str>) -> u64 {
    matches!(text, Cow::Borrowed(_)) as u64 * 1000 + text.len() as u64
}

#[export]
fn demo_parse(text: String) -> Result<i64, Error> {
    text.trim()
        .parse()
        .map_err(|e| Error::new(code::INVALID_INPUT, format!("{e}")))
}

#[export]
fn demo_boom(_x: &[u8]) -> Result<(), Error> {
    panic!("kaboom")
}

#[export]
fn demo_nothing() {}

#[derive(crate::Value)]
#[repr(C)]
struct RunSlot {
    changes: i64,
    last_insert_rowid: i64,
}

#[derive(crate::Value)]
#[repr(C)]
struct Reply {
    tags: Array<u8>,
    nums: Array<i64>,
    #[velt(name = "strings")]
    strs: Array<Str>,
}

/// Arrays and structs cross as Velt values (native_abi.md).
#[export]
fn demo_reply(names: &[Str], slot: &RunSlot) -> Result<Reply, Error> {
    Ok(Reply {
        tags: names.iter().map(|n| n.to_str().len() as u8).collect(),
        nums: vec![slot.changes, slot.last_insert_rowid].into(),
        strs: names.iter().rev().cloned().collect(),
    })
}

#[export]
fn demo_run(n: i64) -> RunSlot {
    RunSlot {
        changes: n,
        last_insert_rowid: n * 10,
    }
}

#[export(blocking)]
fn demo_slow(data: Vec<u8>) -> Result<Vec<u8>, Error> {
    Ok(data.iter().rev().copied().collect())
}

/// The synchronous part reads the borrowed arguments; the future owns what it needs.
#[export(async)]
fn demo_send(
    args: &[Str],
) -> impl std::future::Future<Output = Result<u64, Error>> + Send + 'static {
    let n = args.iter().map(|a| a.as_bytes().len()).sum::<usize>() as u64;
    async move { Ok(n) }
}

fn sig(record: &[u8]) -> &str {
    std::str::from_utf8(record.strip_suffix(&[0]).unwrap()).unwrap()
}

#[test]
fn signature_records() {
    assert_eq!(sig(&__VELT_SIG_demo_add), "(u64,i32)->u64");
    assert_eq!(sig(&__VELT_SIG_demo_greet), "(string,bool)->string");
    assert_eq!(sig(&__VELT_SIG_demo_parse), "(string)->IoResult<i64>");
    assert_eq!(sig(&__VELT_SIG_demo_borrowed), "(string)->u64");
    assert_eq!(sig(&__VELT_SIG_demo_boom), "(u8[])->IoStatus");
    assert_eq!(sig(&__VELT_SIG_demo_nothing), "()->void");
    assert_eq!(
        sig(&__VELT_SIG_demo_reply),
        "(string[],{changes:i64,lastInsertRowid:i64})->IoResult<{tags:u8[],nums:i64[],strings:string[]}>"
    );
    assert_eq!(
        sig(&__VELT_SIG_demo_run),
        "(i64)->{changes:i64,lastInsertRowid:i64}"
    );
    assert_eq!(sig(&__VELT_SIG_demo_slow), "async (u8[])->IoResult<u8[]>");
    assert_eq!(
        sig(&__VELT_SIG_demo_send),
        "async (string[])->IoResult<u64>"
    );
}

#[test]
fn sync_exports() {
    init();
    unsafe {
        assert_eq!(demo_add(40, 2), 42);
        let name = velt_str("velt");
        let mut out = VeltStr::empty();
        demo_greet(&name, true, &mut out);
        assert_eq!(rust_str(&out), "hello, velt!");

        let mut r = MaybeUninit::<IoResultSlot<i64>>::uninit();
        demo_parse(&velt_str(" 17 "), r.as_mut_ptr());
        let r = r.assume_init();
        assert_eq!((r.err.code, r.value.assume_init()), (0, 17));

        let mut r = MaybeUninit::<IoResultSlot<i64>>::uninit();
        demo_parse(&velt_str("x"), r.as_mut_ptr());
        let r = r.assume_init();
        assert_eq!(r.err.code, code::INVALID_INPUT);
        assert!(rust_str(&r.err.message).contains("invalid digit"));
    }
}

/// A Velt string with a lone surrogate (the runtime stores it as WTF-8).
fn lone_str() -> VeltStr {
    let bytes = [b"a".as_slice(), &[0xED, 0xA0, 0xBD], b"b"].concat();
    let mut out = VeltStr::empty();
    unsafe { str_new(bytes.as_ptr(), bytes.len(), &mut out) };
    out
}

#[test]
fn lone_surrogates_arrive_as_one_replacement_character() {
    init();
    unsafe {
        let mut out = VeltStr::empty();
        demo_greet(&lone_str(), false, &mut out);
        assert_eq!(rust_str(&out), "hello, a\u{FFFD}b");
        let mut r = MaybeUninit::<IoResultSlot<i64>>::uninit();
        demo_parse(&lone_str(), r.as_mut_ptr());
        assert!(rust_str(&r.assume_init().err.message).contains("invalid digit"));
        // Borrowed when well-formed, a copy of the same length otherwise.
        assert_eq!(demo_borrowed(&velt_str("héllo")), 1006);
        assert_eq!(demo_borrowed(&lone_str()), 5);
    }
    assert_eq!(
        wtf8_to_utf8_lossy(&[0xED, 0xB0, 0x80, b'x', 0xED, 0x9F, 0xBF]),
        "\u{FFFD}x\u{D7FF}"
    );
}

#[test]
fn panics_become_errors() {
    init();
    let bytes = Array::<u8>::new();
    let mut out = MaybeUninit::<VeltErr>::uninit();
    unsafe {
        demo_boom(arg(&bytes), out.as_mut_ptr());
        let e = out.assume_init();
        assert_eq!(e.code, code::OTHER);
        assert_eq!(rust_str(&e.message), "native panic: kaboom");
    }
}

#[test]
fn blocking_exports_copy_their_arguments() {
    init();
    let bytes = mock_bytes(&[1, 2, 3]);
    unsafe {
        let fut = demo_slow(arg(&bytes)) as *mut u8;
        let r = &*(fut.add(16) as *const IoResultSlot<Array<u8>>);
        assert_eq!(r.err.code, 0);
        assert_eq!(rust_bytes(r.value.assume_init_ref()), [3, 2, 1]);
    }
}

#[test]
fn a_dropped_completer_completes_with_an_error() {
    init();
    // The mock's future is null, so Drop has no future to free; it must still complete.
    drop(Completer::<Result<(), Error>>::new());
    let (handle, code, message) = COMPLETED.lock().unwrap().take().unwrap();
    assert_eq!((handle, code), (7, code::OTHER));
    assert_eq!(message, "completer dropped");

    // Completed explicitly: exactly once, with the value.
    Completer::<Result<(), Error>>::new().complete(Err(Error::new(code::TIMED_OUT, "late")));
    let (_, code, message) = COMPLETED.lock().unwrap().take().unwrap();
    assert_eq!((code, message.as_str()), (code::TIMED_OUT, "late"));
    assert!(COMPLETED.lock().unwrap().is_none());
}

#[test]
fn arrays_and_structs_cross_as_velt_values() {
    init();
    let names: Array<Str> = vec!["ab".to_string(), "cde".to_string()].into();
    let slot = RunSlot {
        changes: 2,
        last_insert_rowid: 7,
    };
    let mut out = MaybeUninit::<IoResultSlot<Reply>>::uninit();
    unsafe {
        demo_reply(
            &names as *const Array<Str> as *const crate::value::RawArray<Str>,
            &slot,
            out.as_mut_ptr(),
        );
        let r = out.assume_init();
        assert_eq!(r.err.code, 0);
        let reply = r.value.assume_init();
        assert_eq!(reply.tags.as_slice(), [2, 3]);
        assert_eq!(reply.nums.as_slice(), [2, 7]);
        let strs: Vec<String> = reply.strs.iter().map(|s| s.to_str().into_owned()).collect();
        assert_eq!(strs, ["cde", "ab"]);
        let mut run = MaybeUninit::<RunSlot>::uninit();
        demo_run(4, run.as_mut_ptr());
        let run = run.assume_init();
        assert_eq!((run.changes, run.last_insert_rowid), (4, 40));
    }
}

#[test]
fn strbuf_builds_short_and_long_strings_in_place() {
    init();
    let mut b = StrBuf::new();
    b.push_str("short");
    assert_eq!(b.finish().to_str(), "short");
    let mut b = StrBuf::new();
    b.push_str("0123456789abcdefghij");
    // Past the inline size: the small text moves into the runtime's buffer, then it grows.
    for _ in 0..100 {
        b.push_str("é-");
    }
    let s = b.finish();
    assert_eq!(
        s.to_str(),
        format!("0123456789abcdefghij{}", "é-".repeat(100))
    );
    let mut b = StrBuf::with_capacity(4);
    b.reserve(1000);
    b.push('x');
    assert_eq!(b.finish().to_str(), "x");
    // Dropped unfinished: the buffer is released.
    let mut b = StrBuf::new();
    b.push_str(&"y".repeat(100));
    drop(b);
}

#[test]
fn a_cloned_str_keeps_its_text() {
    init();
    let s = Str::new("shared text, longer than inline");
    let t = s.clone();
    drop(s);
    assert_eq!(t.as_ref(), b"shared text, longer than inline");
}
