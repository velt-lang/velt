//! The `extern "C"` surface of the JSON pull reader (`velt_rt_json_reader_*`,
//! `velt_rt_json_error`). Return conventions: 1 = ok / 0 = error, except `next_key` and
//! `array_next` (0 = end, 1 = more, 2 = error) and `peek` (a `TOKEN_*` kind).

use super::reader::{Reader, STEP_END, STEP_ERROR, STEP_MORE};
use super::value_abi::ValueHandle;
use crate::handle::Handle;
use crate::str::VeltStr;

/// `new Reader(src)`: never null. `src` must stay alive and unchanged until `reader_free`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_new(src: *const VeltStr) -> *mut Reader {
    // The reader keeps the slice: `src` (and so its bytes, also when stored inline) must stay
    // put until `reader_free`, which generated decoders guarantee.
    velt_rt_json_reader_new_with(src, 0, 0)
}

/// `new Reader(src)` with options: `flags` (1 = fail on object keys the target type does
/// not have) and the deepest nesting allowed (`max_depth`, 0 = no limit).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_new_with(
    src: *const VeltStr,
    flags: u32,
    max_depth: u32,
) -> *mut Reader {
    let bytes: &'static [u8] = std::mem::transmute::<&[u8], &'static [u8]>((*src).as_bytes());
    let lone_free = (*src).is_well_formed();
    Box::into_raw(Box::new(Reader::with_source(
        bytes, lone_free, flags, max_depth,
    )))
}

/// Free the reader (not the source).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_free(r: *mut Reader) {
    if !r.is_null() {
        drop(Box::from_raw(r));
    }
}

/// Kind of the next token (`TOKEN_*`: 0 EOF … 10 error); consumes nothing but whitespace.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_peek(r: *mut Reader) -> u32 {
    (*r).peek()
}

/// Consume `{`: 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_expect_object_start(r: *mut Reader) -> u8 {
    (*r).open(b'{')
}

/// Consume `[`: 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_expect_array_start(r: *mut Reader) -> u8 {
    (*r).open(b'[')
}

/// 1 = `*out` is the next key (the value follows), 0 = end of object (`}` consumed),
/// 2 = error. The key borrows the source when it has no escapes (`cap == 0`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_next_key(r: *mut Reader, out: *mut VeltStr) -> u8 {
    let r = &mut *r;
    match r.next_key() {
        Ok(Some(key)) => {
            out.write(r.borrowed_str(key));
            STEP_MORE
        }
        Ok(None) => STEP_END,
        Err(()) => STEP_ERROR,
    }
}

/// 1 = another element follows, 0 = end of array (`]` consumed), 2 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_array_next(r: *mut Reader) -> u8 {
    (*r).array_next()
}

/// Read a string into `*out` (owned): 1 = ok, 0 = error (`out` untouched).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_string(r: *mut Reader, out: *mut VeltStr) -> u8 {
    let r = &mut *r;
    match r.string() {
        Some(tok) => {
            out.write(r.owned_str(tok));
            1
        }
        None => 0,
    }
}

/// Read a number: 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_f64(r: *mut Reader, out: *mut f64) -> u8 {
    (*r).f64().map_or(0, |v| {
        out.write(v);
        1
    })
}

/// Read an integral number that fits `i64` exactly (`3`, `3.0`, `3e2`): 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_i64(r: *mut Reader, out: *mut i64) -> u8 {
    (*r).i64().map_or(0, |v| {
        out.write(v);
        1
    })
}

/// Read `true`/`false` into `*out` (0/1): 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_bool(r: *mut Reader, out: *mut u8) -> u8 {
    (*r).bool().map_or(0, |v| {
        out.write(v as u8);
        1
    })
}

/// Read `null`: 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_null(r: *mut Reader) -> u8 {
    (*r).null()
}

/// Skip one value of any kind: 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_skip_value(r: *mut Reader) -> u8 {
    (*r).skip()
}

/// Skip one value like `skip_value`, remembering where its arrays/objects end so that the next
/// `skip_lookahead` at one of them jumps to its end (union decoders looking ahead): 1 = ok,
/// 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_skip_lookahead(r: *mut Reader) -> u8 {
    (*r).skip_lookahead()
}

/// The reader's position, for `reset` (looking ahead and coming back).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_mark(r: *const Reader) -> u64 {
    (*r).mark()
}

/// Go back to a position from `mark` (of the same reader), clearing any error since.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_reset(r: *mut Reader, mark: u64) {
    (*r).reset(mark)
}

/// Read one value of any kind into `*out` (an owned `json.Value` handle): 1 = ok, 0 = error
/// (`out` untouched).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_value(
    r: *mut Reader,
    out: *mut ValueHandle,
) -> u8 {
    (*r).value().map_or(0, |v| {
        out.write(Handle::from_arc(v));
        1
    })
}

/// Skip the value of an object key the target type does not have: 1 = ok, 0 = error (also
/// when the reader rejects unknown keys: then the message is `unknown field at <path>`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_skip_unknown(r: *mut Reader) -> u8 {
    (*r).skip_unknown()
}

/// After the top-level value: 1 if only whitespace remains, else 0 (error recorded).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_end(r: *mut Reader) -> u8 {
    (*r).end()
}

/// Build the `JsonError.message` into `*out` (owned). `expected` names what the decoder
/// wanted (`string`, `i64`, `object`, `field "age"`…), `path` where (`$.tags[1]`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_error(
    r: *const Reader,
    expected: *const VeltStr,
    path: *const VeltStr,
    out: *mut VeltStr,
) {
    let expected = (*expected).text_lossy();
    let path = (*path).text_lossy();
    out.write(VeltStr::from_vec(
        (*r).message(&expected, &path).into_bytes(),
    ));
}
