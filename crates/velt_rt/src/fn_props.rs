//! Properties of function values (rt_abi.md "Function properties", #924): JavaScript functions
//! are objects, and a program may add properties to one it holds as `any`
//! (`factory.__setup = setup`). A function's identity is its closure environment (every function
//! value that may get properties has a counted one), so the properties live in a table keyed by
//! the environment's address, in insertion order, each value an `any` (`{ data, table }`).
//!
//! An environment with properties has its drop entry (its first word) replaced by
//! [`velt_rt_fnprop_env_drop`], which runs the original drop and, with the last reference,
//! removes the entry and drops the values first. Functions that never get properties are not in
//! the table and run no code of this module.
//!
//! One table for the process, behind a lock: a task moves between worker threads at an `await`.
//! The values are dropped and the original drop runs outside the lock, since either may drop
//! another function with properties.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use crate::str::VeltStr;

/// An `any` value as generated code lays it out: the data pointer and the type's table, each in
/// a 64-bit word on every target (a pointer is 8 bytes in compiled code's memory, wasm32 too).
type AnyValue = [u64; 2];

/// An environment's drop entry.
type DropFn = unsafe extern "C" fn(*mut u8);

/// Byte offset of a table's drop entry (`SLOT_DROP`, the slot just in front of slot 0, of the
/// seven hidden ones: velt_vir `glue/vtable.rs`).
const TABLE_DROP: usize = 8 * 6;

/// Byte offset of a table's share entry (`SLOT_SHARE`): `(data) -> data` of another reference.
const TABLE_SHARE: usize = 8 * 3;

/// Byte offset of a table's transfer entry (`SLOT_TRANSFER`): `(data) -> data` safe on another
/// thread.
const TABLE_TRANSFER: usize = 8;

/// An entry `(data) -> data` of an `any` value's table.
type DataFn = unsafe extern "C" fn(*mut u8) -> *mut u8;

/// An environment's transfer entry.
type TransferFn = unsafe extern "C" fn(*mut u8) -> *mut u8;

/// The count word in front of a counted object holds the count in its low 62 bits.
const COUNT_MASK: u64 = (1 << 62) - 1;

struct Props {
    /// The environment's own drop entry (a word), put back when the entry goes.
    drop: u64,
    /// The environment's own transfer entry (0: none), put back when the entry goes.
    transfer: u64,
    /// The properties in insertion order, each value boxed so its address stays put while
    /// generated code shares it.
    entries: Vec<(VeltStr, Box<AnyValue>)>,
}

// SAFETY: the table is only reached under its lock, and the strings and values in it belong to
// the function, which one thread uses at a time (counted objects never run on two at once).
unsafe impl Send for Props {}

static TABLE: Mutex<Option<HashMap<usize, Props>>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut HashMap<usize, Props>) -> R) -> R {
    let mut t = TABLE.lock().unwrap_or_else(PoisonError::into_inner);
    f(t.get_or_insert_with(HashMap::new))
}

/// Drop the `any` value `v` through its table.
unsafe fn drop_any(v: AnyValue) {
    if v[1] == 0 {
        return;
    }
    if let Some(drop) = fn_word::<DropFn>(*((v[1] as usize + TABLE_DROP) as *const u64)) {
        drop(v[0] as usize as *mut u8);
    }
}

/// The entry at byte `offset` of the table of the `any` value `v`, if it has one.
unsafe fn table_entry(v: &AnyValue, offset: usize) -> Option<DataFn> {
    if v[1] == 0 {
        return None;
    }
    fn_word::<DataFn>(*((v[1] as usize + offset) as *const u64))
}

/// The function a word of compiled code's memory holds (0: none).
unsafe fn fn_word<F: Copy>(w: u64) -> Option<F> {
    if w == 0 {
        return None;
    }
    let p = w as usize;
    Some(std::mem::transmute_copy::<usize, F>(&p))
}

/// Word `i` of an environment's header (drop, clone, transfer), 8 bytes each.
unsafe fn header(env: *mut u8, i: usize) -> *mut u64 {
    env.add(8 * i) as *mut u64
}

/// The word of compiled code's memory that holds the address of function `f`.
fn word_of(f: *const ()) -> u64 {
    f as usize as u64
}

/// `f(v[0])`: an `any` value's data through a `(data) -> data` entry.
unsafe fn through(f: DataFn, v: &mut AnyValue) {
    v[0] = f(v[0] as usize as *mut u8) as usize as u64;
}

/// Put the runtime's drop and transfer entries into `env`, a new entry of the table.
unsafe fn adopt(env: *mut u8, entries: Vec<(VeltStr, Box<AnyValue>)>) -> Props {
    let drop = *header(env, 0);
    *header(env, 0) = word_of(velt_rt_fnprop_env_drop as *const ());
    let transfer = *header(env, 2);
    *header(env, 2) = word_of(velt_rt_fnprop_env_transfer as *const ());
    Props {
        drop,
        transfer,
        entries,
    }
}

unsafe fn find(entries: &[(VeltStr, Box<AnyValue>)], key: *const VeltStr) -> Option<usize> {
    let k = (*key).as_bytes();
    entries.iter().position(|(n, _)| n.as_bytes() == k)
}

/// The value of property `key` of the function whose environment is `env`, borrowed (the
/// caller shares it before anything else runs), or null when it has none.
///
/// # Safety
/// `env` is a live environment and `key` a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fnprop_get(env: *const u8, key: *const VeltStr) -> *const u8 {
    with(|t| match t.get(&(env as usize)) {
        Some(p) => match find(&p.entries, key) {
            Some(i) => &*p.entries[i].1 as *const AnyValue as *const u8,
            None => std::ptr::null(),
        },
        None => std::ptr::null(),
    })
}

/// Set property `key` of the function whose environment (a counted one) is `env` to the `any`
/// at `v`, which this takes; the value it replaces is dropped after the store.
///
/// # Safety
/// `env` is a live counted environment, `key` a valid string, `v` an owned `any`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fnprop_set(env: *mut u8, key: *const VeltStr, v: *const u8) {
    let value = *(v as *const AnyValue);
    let old = with(|t| {
        // The environment now drops and transfers its properties too.
        let props = t.entry(env as usize).or_insert_with(|| adopt(env, vec![]));
        match find(&props.entries, key) {
            Some(i) => Some(std::mem::replace(&mut *props.entries[i].1, value)),
            None => {
                let mut name = VeltStr::empty();
                crate::str::velt_rt_str_own(key, &mut name);
                props.entries.push((name, Box::new(value)));
                None
            }
        }
    });
    if let Some(old) = old {
        drop_any(old);
    }
}

/// Remove property `key` of the function whose environment is `env`: 1 if it had one.
///
/// # Safety
/// `env` is a live environment and `key` a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fnprop_delete(env: *const u8, key: *const VeltStr) -> u8 {
    let gone = with(|t| {
        let p = t.get_mut(&(env as usize))?;
        let i = find(&p.entries, key)?;
        Some(p.entries.remove(i))
    });
    match gone {
        Some((mut name, v)) => {
            name.release();
            drop_any(*v);
            1
        }
        None => 0,
    }
}

/// How many properties the function whose environment is `env` has.
///
/// # Safety
/// `env` is a live environment.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fnprop_len(env: *const u8) -> u64 {
    with(|t| t.get(&(env as usize)).map_or(0, |p| p.entries.len() as u64))
}

/// The name of property `i` (in insertion order) of the function whose environment is `env`,
/// written to `out` (owned).
///
/// # Safety
/// `env` is a live environment with more than `i` properties; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fnprop_key(env: *const u8, i: u64, out: *mut VeltStr) {
    let name = with(|t| {
        t.get(&(env as usize))
            .and_then(|p| p.entries.get(i as usize))
            .map(|(n, _)| n.share())
    });
    out.write(name.unwrap_or_else(VeltStr::empty));
}

/// The size of an identity environment's block: the count (a `u64` on every target), then the
/// drop, clone and transfer entries of every environment's header (pointer-sized).
const IDENTITY_BLOCK: u64 = 32;

/// The identity environment of each function value that has none of its own, by its code
/// address: a declared function is one object in JavaScript, whichever reference reaches it.
/// The table holds one reference to each, for the life of the process, as the function lives.
static IDENTITIES: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);

/// The identity environment of the function value with code `code` and no environment, made
/// on first use (an environment holding nothing), with one more reference for the caller: the
/// function value a property is written to keeps it as its environment.
#[no_mangle]
pub extern "C" fn velt_rt_fnprop_identity(code: *const u8) -> *mut u8 {
    // A generic closure's value has no code of its own to name it by.
    if code.is_null() {
        return std::ptr::null_mut();
    }
    let mut t = IDENTITIES.lock().unwrap_or_else(PoisonError::into_inner);
    let env = *t
        .get_or_insert_with(HashMap::new)
        .entry(code as usize)
        .or_insert_with(|| empty_env() as usize) as *mut u8;
    // SAFETY: the table's reference keeps the environment alive.
    unsafe { *(env.sub(8) as *mut u64) += 1 };
    env
}

/// A new environment holding nothing, counted once.
fn empty_env() -> *mut u8 {
    let p = crate::mem::velt_rt_alloc(IDENTITY_BLOCK, 8);
    // SAFETY: a fresh block of the count and three pointer-sized words.
    unsafe {
        (p as *mut u64).write(1);
        let env = p.add(8);
        *header(env, 0) = word_of(identity_drop as *const ());
        *header(env, 1) = word_of(identity_clone as *const ());
        *header(env, 2) = 0;
        env
    }
}

/// The identity environment of the function value with code `code` and no environment, if a
/// property was ever written to it (not counted: the table's reference keeps it), or null.
#[no_mangle]
pub extern "C" fn velt_rt_fnprop_identity_of(code: *const u8) -> *mut u8 {
    let t = IDENTITIES.lock().unwrap_or_else(PoisonError::into_inner);
    let env = t.as_ref().and_then(|t| t.get(&(code as usize)).copied());
    env.unwrap_or(0) as *mut u8
}

/// The drop entry of an identity environment: one reference less, the block freed with the last.
unsafe extern "C" fn identity_drop(env: *mut u8) {
    let count = env.sub(8) as *mut u64;
    if *count & COUNT_MASK == 1 {
        crate::mem::velt_rt_free(env.sub(8), IDENTITY_BLOCK, 8);
    } else {
        *count -= 1;
    }
}

/// The clone entry of an identity environment: it holds nothing to copy, so a copy of the
/// function is the same function, as in JavaScript.
unsafe extern "C" fn identity_clone(env: *mut u8) -> *mut u8 {
    *(env.sub(8) as *mut u64) += 1;
    env
}

/// The drop entry of an environment with properties: with its last reference the entry goes
/// and its values are dropped; then the environment's own drop runs.
///
/// # Safety
/// `env` is a live counted environment in the table.
pub unsafe extern "C" fn velt_rt_fnprop_env_drop(env: *mut u8) {
    let count = *(env.sub(8) as *const u64) & COUNT_MASK;
    let (drop, gone) = with(|t| {
        let key = env as usize;
        let Some(p) = t.get(&key) else {
            crate::panic::fatal("ICE: a function's properties are missing");
        };
        let drop = p.drop;
        if count != 1 {
            return (drop, None);
        }
        let p = t.remove(&key);
        *header(env, 0) = drop;
        if let Some(p) = &p {
            *header(env, 2) = p.transfer;
        }
        (drop, p.map(|p| p.entries))
    });
    for (mut name, v) in gone.into_iter().flatten() {
        name.release();
        drop_any(*v);
    }
    if let Some(drop) = fn_word::<DropFn>(drop) {
        drop(env);
    }
}

/// The transfer entry of an environment with properties (a function value about to enter
/// another thread, velt_vir `glue/transfer_env.rs`): the properties go with the function as an
/// object's members do. A unique environment moves, its values transferred in place; a shared
/// one is copied (by its own transfer entry, or as a new environment holding nothing when it has
/// none), and the copy gets a transferred copy of each property. Called with the pointer tagged
/// (the many-threads check), it hands the check to the environment's own entry.
///
/// # Safety
/// `env` is a live counted environment in the table (tagged or not).
pub unsafe extern "C" fn velt_rt_fnprop_env_transfer(env: *mut u8) -> *mut u8 {
    let plain = (env as usize & !1) as *mut u8;
    let orig = with(|t| t.get(&(plain as usize)).map_or(0, |p| p.transfer));
    let orig = fn_word::<TransferFn>(orig);
    if env as usize & 1 != 0 {
        return match orig {
            Some(f) => f(env),
            None => plain,
        };
    }
    let count = *(plain.sub(8) as *const u64) & COUNT_MASK;
    if count == 1 {
        // It moves: the values go along, each transferred in place.
        let new = match orig {
            Some(f) => f(plain),
            None => plain,
        };
        let values: Vec<AnyValue> = with(|t| {
            t.get(&(plain as usize))
                .map_or(vec![], |p| p.entries.iter().map(|e| *e.1).collect())
        });
        let moved: Vec<AnyValue> = (values.into_iter())
            .map(|mut v| {
                if let Some(f) = table_entry(&v, TABLE_TRANSFER) {
                    through(f, &mut v);
                }
                v
            })
            .collect();
        with(|t| {
            if let Some(p) = t.get_mut(&(plain as usize)) {
                for (e, v) in p.entries.iter_mut().zip(moved) {
                    *e.1 = v;
                }
            }
        });
        return new;
    }
    // Shared: a copy of the function for the other thread, with copies of its properties.
    let copies: Vec<(VeltStr, AnyValue)> = with(|t| {
        t.get(&(plain as usize)).map_or(vec![], |p| {
            p.entries.iter().map(|(n, v)| (n.share(), **v)).collect()
        })
    });
    let new = match orig {
        Some(f) => f(plain),
        None => {
            *(plain.sub(8) as *mut u64) -= 1;
            empty_env()
        }
    };
    let entries = (copies.into_iter())
        .map(|(n, mut v)| {
            if let Some(share) = table_entry(&v, TABLE_SHARE) {
                through(share, &mut v);
            }
            if let Some(f) = table_entry(&v, TABLE_TRANSFER) {
                through(f, &mut v);
            }
            (n, Box::new(v))
        })
        .collect();
    let props = adopt(new, entries);
    with(|t| t.insert(new as usize, props));
    new
}

/// At exit, for the leak counters only (velt_rt `entry.rs`): the identities of declared
/// functions are released, as module state is, with their properties.
// velt_rt_wasm compiles this file too, without the leak counters' exit.
#[allow(dead_code)]
pub(crate) fn drop_at_exit() {
    let envs: Vec<usize> = {
        let mut t = IDENTITIES.lock().unwrap_or_else(PoisonError::into_inner);
        t.take().map_or(vec![], |t| t.into_values().collect())
    };
    for env in envs {
        // SAFETY: the table held one reference to each; its first word is its drop entry.
        unsafe {
            if let Some(drop) = fn_word::<DropFn>(*header(env as *mut u8, 0)) {
                drop(env as *mut u8);
            }
        }
    }
}
