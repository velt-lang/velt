//! Symbols (rt_abi.md "Symbols"): a `symbol` value is the address of a [`SymbolRec`], compared
//! by identity. Records live as long as the program: those of module constants
//! (`const KEY = Symbol("k")`, `const R = Symbol.for("r")`) and of the well-known symbols are
//! read-only data the compiler emits, `Symbol(desc)` at run time leaks one here (one 32-byte
//! allocation, the description inside it), and `Symbol.for(key)` keeps one per key in a
//! process-wide registry: the compiler's record for a key when the program has one (its
//! registry table), else one made here on first use.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::str::VeltStr;
use crate::strbuf::{velt_rt_strbuf_push_bytes, VeltStrBuf};

/// A symbol: its description and what made it.
#[repr(C)]
pub struct SymbolRec {
    /// The description (empty without one: [`DESCRIBED`] tells `Symbol()` from `Symbol("")`).
    pub desc: VeltStr,
    /// [`KEY_FRESH`], [`KEY_REGISTERED`] (also a registry record the compiler emitted), or the
    /// compiler's number for another record it emitted (2 and up; unique per record, so no two
    /// records have the same bytes), with [`DESCRIBED`] set when it has a description.
    pub key: u64,
}

const _: () = assert!(std::mem::size_of::<SymbolRec>() == 32);

/// Made by `Symbol(desc)` at run time.
pub const KEY_FRESH: u64 = 0;
/// In the registry (`Symbol.for(key)`): `Symbol.keyFor` finds its key.
pub const KEY_REGISTERED: u64 = 1;
/// The bit of `key` set when the symbol has a description.
pub const DESCRIBED: u64 = 1 << 63;

/// One slot of the compiler's registry table (8 bytes; on wasm32 the address takes the first
/// half).
#[repr(C, align(8))]
pub struct RegistrySlot(pub *const SymbolRec);

/// `Symbol.for` keys → record addresses.
static REGISTRY: Mutex<Option<HashMap<Vec<u8>, usize>>> = Mutex::new(None);

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

/// `Symbol.for(key)`: the registry's symbol for `key`. On first use of a key it is the record
/// in the compiler's `table` with that description (null-terminated slots), or a new one.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_symbol_for(
    key: *const VeltStr,
    table: *const RegistrySlot,
) -> *const SymbolRec {
    let bytes = (*key).as_bytes();
    let mut reg = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    let map = reg.get_or_insert_with(HashMap::new);
    if let Some(&rec) = map.get(bytes) {
        return rec as *const SymbolRec;
    }
    let rec = match compiled_record(table, bytes) {
        Some(rec) => rec,
        None => leak(Some((*key).share()), KEY_REGISTERED),
    };
    map.insert(bytes.to_vec(), rec as usize);
    rec
}

/// The record in the compiler's registry `table` whose description is `key`.
unsafe fn compiled_record(table: *const RegistrySlot, key: &[u8]) -> Option<*const SymbolRec> {
    if table.is_null() {
        return None;
    }
    let mut slot = table;
    loop {
        let rec = (*slot).0;
        if rec.is_null() {
            return None;
        }
        if (*rec).desc.as_bytes() == key {
            return Some(rec);
        }
        slot = slot.add(1);
    }
}

/// Was `s` made by `Symbol.for` (so `Symbol.keyFor(s)` is its description)? 1 or 0.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_symbol_registered(s: *const SymbolRec) -> u8 {
    ((*s).key & !DESCRIBED == KEY_REGISTERED) as u8
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
            let none_table = std::ptr::null();
            let (f, g) = (velt_rt_symbol_for(&d, none_table), velt_rt_symbol_for(&d, none_table));
            assert_eq!(f, g);
            assert_ne!(f, a);
            assert_eq!(velt_rt_symbol_registered(f), 1);
            assert_eq!(text(f), "k");
        }
    }

    #[test]
    fn the_registry_gives_the_compilers_record_for_its_key() {
        let key = VeltStr::from_vec(b"compiled-key".to_vec());
        // SAFETY: a valid string.
        let rec = SymbolRec {
            desc: unsafe { key.share() },
            key: KEY_REGISTERED | DESCRIBED,
        };
        let table = [RegistrySlot(&rec), RegistrySlot(std::ptr::null())];
        let other = VeltStr::from_vec(b"compiled-other".to_vec());
        // SAFETY: valid strings, a null-terminated table of a live record.
        unsafe {
            assert_eq!(velt_rt_symbol_for(&key, table.as_ptr()), &rec as *const SymbolRec);
            assert_eq!(velt_rt_symbol_for(&key, std::ptr::null()), &rec as *const SymbolRec);
            let made = velt_rt_symbol_for(&other, table.as_ptr());
            assert_ne!(made, &rec as *const SymbolRec);
            assert_eq!(text(made), "compiled-other");
        }
    }
}
