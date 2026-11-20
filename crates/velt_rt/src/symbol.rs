//! Symbols (rt_abi.md "Symbols"): a `symbol` value is the address of a [`SymbolRec`], compared
//! by identity. Records live as long as the program: those of module constants
//! (`const KEY = Symbol("k")`, `const R = Symbol.for("r")`) and of the well-known symbols are
//! read-only data the compiler emits, `Symbol(desc)` at run time leaks one here (one 32-byte
//! allocation, the description inside it), and `Symbol.for(key)`'s registry is std's
//! (std/lang/symbol/registry.velt), which makes its records itself with the same layout.

use crate::str::VeltStr;
use crate::strbuf::{velt_rt_strbuf_push_bytes, VeltStrBuf};

/// A symbol: its description and what made it.
#[repr(C)]
pub struct SymbolRec {
    /// The description (empty without one: [`DESCRIBED`] tells `Symbol()` from `Symbol("")`).
    pub desc: VeltStr,
    /// [`KEY_FRESH`], [`KEY_REGISTERED`] (a registry record, the compiler's or std's), or the
    /// compiler's number for another record it emitted (2 and up; unique per record, so no two
    /// records have the same bytes), with [`DESCRIBED`] set when it has a description.
    pub key: u64,
}

const _: () = assert!(std::mem::size_of::<SymbolRec>() == 32);

/// Made by `Symbol(desc)` at run time.
pub const KEY_FRESH: u64 = 0;
/// In the registry (`Symbol.for(key)`): `Symbol.keyFor` finds its key (std reads it,
/// std/lang/symbol/registry.velt's `KEY_REGISTERED`).
pub const KEY_REGISTERED: u64 = 1;
/// The bit of `key` set when the symbol has a description.
pub const DESCRIBED: u64 = 1 << 63;

fn leak(desc: Option<VeltStr>, key: u64) -> *const SymbolRec {
    let rec = match desc {
        Some(desc) => SymbolRec {
            desc,
            key: key | DESCRIBED,
        },
        None => SymbolRec {
            desc: VeltStr::empty(),
            key,
        },
    };
    Box::into_raw(Box::new(rec))
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

/// Does `s` have a description? 1 or 0.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_symbol_described(s: *const SymbolRec) -> u8 {
    ((*s).key & DESCRIBED != 0) as u8
}

/// The description of `s` (`""` when it has none).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_symbol_description(s: *const SymbolRec, out: *mut VeltStr) {
    out.write((*s).desc.share());
}

/// Append `Symbol(<description>)`, as `String(s)` and `console.log` write a symbol.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_push_symbol(buf: *mut VeltStrBuf, s: *const SymbolRec) {
    push_ascii(buf, b"Symbol(");
    (*buf).push_str(&(*s).desc);
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
    fn fresh_symbols_differ() {
        let d = VeltStr::from_vec(b"k".to_vec());
        // SAFETY: valid strings and the records these calls return.
        unsafe {
            let (a, b) = (velt_rt_symbol_new(&d, 1), velt_rt_symbol_new(&d, 1));
            assert_ne!(a, b);
            assert_eq!(text(a), "k");
            assert_eq!((*a).key, KEY_FRESH | DESCRIBED);
            let none = velt_rt_symbol_new(std::ptr::null(), 0);
            assert_eq!(velt_rt_symbol_described(none), 0);
            assert_eq!(text(none), "");
        }
    }
}
