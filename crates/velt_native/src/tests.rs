//! The SDK against a mock function table: argument conversion, results, panics and the signature
//! records `velt native build` reads.

use super::*;
use std::sync::Once;

// Mock strings: words[0] = pointer to a leaked `Vec<u8>`, words[1] = length.
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
    len.write(v.len());
    v.as_ptr()
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
fn mock_bytes(b: &[u8]) -> VeltBytes {
    new_bytes(b)
}
fn rust_bytes(b: &VeltBytes) -> Vec<u8> {
    unsafe { bytes_of(b) }.to_vec()
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

static MOCK: Api = Api {
    abi_version: 1,
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

#[export(blocking)]
fn demo_slow(data: Vec<u8>) -> Result<Vec<u8>, Error> {
    Ok(data.iter().rev().copied().collect())
}

fn sig(record: &[u8]) -> &str {
    std::str::from_utf8(record.strip_suffix(&[0]).unwrap()).unwrap()
}

#[test]
fn signature_records() {
    assert_eq!(sig(&__VELT_SIG_demo_add), "(u64,i32)->u64");
    assert_eq!(sig(&__VELT_SIG_demo_greet), "(string,bool)->string");
    assert_eq!(sig(&__VELT_SIG_demo_parse), "(string)->IoResult<i64>");
    assert_eq!(sig(&__VELT_SIG_demo_boom), "(u8[])->IoStatus");
    assert_eq!(sig(&__VELT_SIG_demo_nothing), "()->void");
    assert_eq!(sig(&__VELT_SIG_demo_slow), "async (u8[])->IoResult<u8[]>");
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

#[test]
fn panics_become_errors() {
    init();
    let bytes = VeltBytes::empty();
    let mut out = MaybeUninit::<VeltErr>::uninit();
    unsafe {
        demo_boom(&bytes, out.as_mut_ptr());
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
        let fut = demo_slow(&bytes) as *mut u8;
        let r = &*(fut.add(16) as *const IoResultSlot<VeltBytes>);
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
