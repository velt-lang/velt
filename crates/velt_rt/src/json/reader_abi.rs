//! The `extern "C"` surface of the JSON pull reader (`velt_rt_json_reader_*`,
//! `velt_rt_json_error`). Return conventions: 1 = ok / 0 = error, except `next_key` and
//! `array_next` (0 = end, 1 = more, 2 = error) and `peek` (a `TOKEN_*` kind). The reader is
//! passed as a `u64` handle (its address): Velt code holds it as one (std/json/reader.vlt), and
//! on wasm32 a pointer parameter would not match that `u64`.

use super::reader::{Reader, FLAG_CURSOR, FLAG_ROWS, STEP_END, STEP_ERROR, STEP_MORE};
use super::rows_reader::RowReader;
use super::value_abi::ValueHandle;
use crate::bytes::VeltBytes;
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
/// not have; 4 = `src` is a `u8[]` of binary rows, `rows_bin.rs`; 8 = `src` is a driver's
/// live rows, a `velt_text::rows_cursor::RawCursor`, which the reader uses and does not free)
/// and the deepest nesting allowed (`max_depth`, 0 = no limit; rows have no nesting to limit).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_new_with(
    src: *const VeltStr,
    flags: u32,
    max_depth: u32,
) -> *mut Reader {
    if flags & FLAG_CURSOR != 0 {
        // A driver's live rows: the caller keeps the cursor until the reader is freed.
        let c = &*(src as *const velt_text::rows_cursor::RawCursor);
        let cursor = velt_text::rows_cursor::RawCursor {
            data: c.data,
            vtable: c.vtable,
        };
        let rows = RowReader::live(cursor, flags);
        return Box::into_raw(Box::new(Reader::over_rows(rows, flags)));
    }
    if flags & FLAG_ROWS != 0 {
        // A driver's binary rows (`rows_bin.rs`) in a `u8[]`, borrowed like a source string.
        let rows = RowReader::borrowed((*(src as *const VeltBytes)).as_bytes(), flags);
        return Box::into_raw(Box::new(Reader::over_rows(rows, flags)));
    }
    let bytes: &'static [u8] = std::mem::transmute::<&[u8], &'static [u8]>((*src).as_bytes());
    let lone_free = (*src).is_well_formed();
    Box::into_raw(Box::new(Reader::with_source(
        bytes, lone_free, flags, max_depth,
    )))
}

/// May a JSON `null` stand for an absent optional field whose type has no `null`? 1 = yes
/// (`JSON.parse<T>`), 0 = no: the decoder reads the field's value and fails
/// (`FLAG_NULL_NOT_ABSENT`, `JSON.parse(text) as T`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_null_absent(h: u64) -> u8 {
    let r = h as usize as *mut Reader;
    u8::from(!(*r).null_not_absent)
}

/// Free the reader (not the source).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_free(h: u64) {
    let r = h as usize as *mut Reader;
    if !r.is_null() {
        drop(Box::from_raw(r));
    }
}

/// Kind of the next token (`TOKEN_*`: 0 EOF … 10 error); consumes nothing but whitespace.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_peek(h: u64) -> u32 {
    let r = h as usize as *mut Reader;
    (*r).peek()
}

/// Consume `{`: 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_expect_object_start(h: u64) -> u8 {
    let r = h as usize as *mut Reader;
    (*r).open(b'{')
}

/// Consume `[`: 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_expect_array_start(h: u64) -> u8 {
    let r = h as usize as *mut Reader;
    (*r).open(b'[')
}

/// 1 = `*out` is the next key (the value follows), 0 = end of object (`}` consumed),
/// 2 = error. The key borrows the source when it has no escapes (`cap == 0`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_next_key(h: u64, out: *mut VeltStr) -> u8 {
    let r = h as usize as *mut Reader;
    let r = &mut *r;
    match r.next_key() {
        Ok(Some(key)) => {
            out.write(r.borrowed_str(key));
            STEP_MORE
        }
        Ok(None) => STEP_END,
        Err(()) => r.rows_or(STEP_ERROR, move |rows| match rows.next_key() {
            Ok(Some(key)) => {
                out.write(key);
                STEP_MORE
            }
            Ok(None) => STEP_END,
            Err(()) => STEP_ERROR,
        }),
    }
}

/// 1 = another element follows, 0 = end of array (`]` consumed), 2 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_array_next(h: u64) -> u8 {
    let r = h as usize as *mut Reader;
    (*r).array_next()
}

/// Read a string into `*out` (owned): 1 = ok, 0 = error (`out` untouched).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_string(h: u64, out: *mut VeltStr) -> u8 {
    let r = h as usize as *mut Reader;
    let r = &mut *r;
    match r.string() {
        Some(tok) => {
            out.write(r.owned_str(tok));
            1
        }
        None => r.rows_or(0, move |rows| {
            rows.string().map_or(0, |s| {
                out.write(s);
                1
            })
        }),
    }
}

/// Read a number: 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_f64(h: u64, out: *mut f64) -> u8 {
    let r = h as usize as *mut Reader;
    let v = match (*r).f64() {
        None => (*r).rows_or(None, RowReader::f64),
        v => v,
    };
    v.map_or(0, |v| {
        out.write(v);
        1
    })
}

/// Read an integral number that fits `i64` exactly (`3`, `3.0`, `3e2`): 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_i64(h: u64, out: *mut i64) -> u8 {
    let r = h as usize as *mut Reader;
    let v = match (*r).i64() {
        None => (*r).rows_or(None, RowReader::i64),
        v => v,
    };
    v.map_or(0, |v| {
        out.write(v);
        1
    })
}

/// Read `true`/`false` into `*out` (0/1): 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_bool(h: u64, out: *mut u8) -> u8 {
    let r = h as usize as *mut Reader;
    let v = match (*r).bool() {
        None => (*r).rows_or(None, RowReader::bool),
        v => v,
    };
    v.map_or(0, |v| {
        out.write(v as u8);
        1
    })
}

/// Read `null`: 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_null(h: u64) -> u8 {
    let r = h as usize as *mut Reader;
    (*r).null()
}

/// Skip one value of any kind: 1 = ok, 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_skip_value(h: u64) -> u8 {
    let r = h as usize as *mut Reader;
    (*r).skip()
}

/// Skip one value like `skip_value`, remembering where its arrays/objects end so that the next
/// `skip_lookahead` at one of them jumps to its end (union decoders looking ahead): 1 = ok,
/// 0 = error.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_skip_lookahead(h: u64) -> u8 {
    let r = h as usize as *mut Reader;
    (*r).skip_lookahead()
}

/// The reader's position, for `reset` (looking ahead and coming back).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_mark(h: u64) -> u64 {
    let r = h as usize as *const Reader;
    // A rows reader always has `error` set; a text reader rarely has when a decoder marks.
    if (*r).error().is_some() && (*r).rows.is_some() {
        // SAFETY: a reader belongs to the one decoder running on it, so nothing else refers
        // to it; the rows reader keeps its marks in a list.
        return (*r.cast_mut()).rows_or(0, RowReader::mark);
    }
    (*r).mark()
}

/// Go back to a position from `mark` (of the same reader), clearing any error since.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_reset(h: u64, mark: u64) {
    let r = h as usize as *mut Reader;
    // As in `mark`: only a failed reader can be a rows reader, whose `error` must stay set.
    if (*r).error().is_some() && (*r).rows.is_some() {
        return (*r).rows_or((), move |rows| rows.reset(mark));
    }
    (*r).reset(mark)
}

/// Read one value of any kind into `*out` (an owned `json.Value` handle): 1 = ok, 0 = error
/// (`out` untouched).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_read_value(h: u64, out: *mut ValueHandle) -> u8 {
    let r = h as usize as *mut Reader;
    let v = match (*r).value() {
        None => (*r).rows_or(None, RowReader::value),
        v => v,
    };
    v.map_or(0, |v| {
        out.write(Handle::from_arc(v));
        1
    })
}

/// Skip the value of an object key the target type does not have: 1 = ok, 0 = error (also
/// when the reader rejects unknown keys: then the message is `unknown field at <path>`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_skip_unknown(h: u64) -> u8 {
    let r = h as usize as *mut Reader;
    (*r).skip_unknown()
}

/// After the top-level value: 1 if only whitespace remains, else 0 (error recorded).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_reader_end(h: u64) -> u8 {
    let r = h as usize as *mut Reader;
    (*r).end()
}

/// Build the `JsonError.message` into `*out` (owned). `expected` names what the decoder
/// wanted (`string`, `i64`, `object`, `field "age"`…), `path` where (`$.tags[1]`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_error(
    h: u64,
    expected: *const VeltStr,
    path: *const VeltStr,
    out: *mut VeltStr,
) {
    let r = h as usize as *const Reader;
    let expected = (*expected).text_lossy();
    let path = (*path).text_lossy();
    out.write(VeltStr::from_vec(
        (*r).message(&expected, &path).into_bytes(),
    ));
}

/// Node's own message for text that is not JSON (`JSON.parse`'s `SyntaxError`), into `*out`:
/// 1 if `src` is not JSON, 0 (`out` untouched) if it is. The Velt reader (std/json/reader.vlt)
/// asks only after a failure.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_node_syntax(src: *const VeltStr, out: *mut VeltStr) -> u8 {
    match super::node_syntax::node_syntax_message((*src).as_bytes()) {
        Some(m) => {
            out.write(VeltStr::from_vec(m.into_bytes()));
            1
        }
        None => 0,
    }
}
