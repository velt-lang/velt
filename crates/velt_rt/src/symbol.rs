//! Symbols (rt_abi.md "Symbols"): a `symbol` value is the address of a [`SymbolRec`], compared
//! by identity. Records live as long as the program: those of module constants
//! (`const KEY = Symbol("k")`) and of the well-known symbols are read-only data the compiler
//! emits, `Symbol(desc)` at run time leaks one here, and `Symbol.for(key)` keeps one per key in a
//! process-wide registry.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::str::VeltStr;
use crate::strbuf::{velt_rt_strbuf_push_bytes, VeltStrBuf};

/// A symbol: its description (null: none, `Symbol()`) and what made it.
#[repr(C)]
pub struct SymbolRec {
    pub desc: *const VeltStr,
    /// [`KEY_FRESH`], [`KEY_REGISTERED`], or the compiler's number for a record it emitted
    /// (2 and up; unique per record, so no two records have the same bytes).
    pub key: u64,
}

/// Made by `Symbol(desc)` at run time.
pub const KEY_FRESH: u64 = 0;
/// Made by `Symbol.for(key)`: in the registry, `Symbol.keyFor` finds its key.
pub const KEY_REGISTERED: u64 = 1;

/// `Symbol.for` keys → record addresses.
static REGISTRY: Mutex<Option<HashMap<Vec<u8>, usize>>> = Mutex::new(None);

fn leak(desc: Option<VeltStr>, key: u64) -> *const SymbolRec {
    let desc = match desc {
        Some(s) => Box::into_raw(Box::new(s)) as *const VeltStr,
        None => std::ptr::null(),
    };
    Box::into_raw(Box::new(SymbolRec { desc, key }))
}

/// `Symbol(desc)` (`described` = 0: `Symbol()`, `desc` is not read): a new symbol.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_symbol_new(
    desc: *const VeltStr,
    described: u8,
) -> *const SymbolRec {
    let desc = (described != 0).then(|| (*desc).share());
    leak(desc, KEY_FRESH)
}

/// `Symbol.for(key)`: the registry's symbol for `key`, made on first use.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_symbol_for(key: *const VeltStr) -> *const SymbolRec {
    let bytes = (*key).as_bytes().to_vec();
    let mut reg = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    let map = reg.get_or_insert_with(HashMap::new);
    if let Some(&rec) = map.get(&bytes) {
        return rec as *const SymbolRec;
    }
    let rec = leak(Some((*key).share()), KEY_REGISTERED);
    map.insert(bytes, rec as usize);
    rec
}

/// Was `s` made by `Symbol.for` (so `Symbol.keyFor(s)` is its description)? 1 or 0.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_symbol_registered(s: *const SymbolRec) -> u8 {
    ((*s).key == KEY_REGISTERED) as u8
}

/// Does `s` have a description? 1 or 0.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_symbol_described(s: *const SymbolRec) -> u8 {
    !(*s).desc.is_null() as u8
}

/// The description of `s` (`""` when it has none).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_symbol_description(s: *const SymbolRec, out: *mut VeltStr) {
    let desc = (*s).desc;
    out.write(match desc.is_null() {
        true => VeltStr::empty(),
        false => (*desc).share(),
    });
}

/// Append `Symbol(<description>)`, as `String(s)` and `console.log` write a symbol.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_symbol(buf: *mut VeltStrBuf, s: *const SymbolRec) {
    push_ascii(buf, b"Symbol(");
    let desc = (*s).desc;
    if !desc.is_null() {
        (*buf).push_str(&*desc);
    }
    push_ascii(buf, b")");
}

unsafe fn push_ascii(buf: *mut VeltStrBuf, text: &[u8]) {
    let n = text.len() as u64;
    velt_rt_strbuf_push_bytes(buf, text.as_ptr(), n | (n << 32));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: *const SymbolRec) -> String {
        let mut out = std::mem::MaybeUninit::<VeltStr>::uninit();
        // SAFETY: a live record; the string is leaked, as in the other rt tests.
        unsafe {
            velt_rt_symbol_description(s, out.as_mut_ptr());
            String::from_utf8_lossy(out.assume_init().as_bytes()).into_owned()
        }
    }

    #[test]
    fn fresh_symbols_differ_and_registered_ones_are_shared() {
        let d = VeltStr::from_vec(b"k".to_vec());
        // SAFETY: valid strings and the records these calls return.
        unsafe {
            let (a, b) = (velt_rt_symbol_new(&d, 1), velt_rt_symbol_new(&d, 1));
            assert_ne!(a, b);
            assert_eq!(text(a), "k");
            assert_eq!(velt_rt_symbol_registered(a), 0);
            let none = velt_rt_symbol_new(std::ptr::null(), 0);
            assert_eq!(velt_rt_symbol_described(none), 0);
            assert_eq!(text(none), "");
            let (f, g) = (velt_rt_symbol_for(&d), velt_rt_symbol_for(&d));
            assert_eq!(f, g);
            assert_ne!(f, a);
            assert_eq!(velt_rt_symbol_registered(f), 1);
            assert_eq!(text(f), "k");
        }
    }
}
