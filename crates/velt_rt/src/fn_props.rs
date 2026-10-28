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

/// An `any` value as generated code lays it out: the data pointer and the type's table.
type AnyValue = [usize; 2];

/// An environment's drop entry.
type DropFn = unsafe extern "C" fn(*mut u8);

/// Byte offset of a table's drop entry (`SLOT_DROP`, the slot just in front of slot 0, of the
/// seven hidden ones: velt_vir `glue/vtable.rs`).
const TABLE_DROP: usize = 8 * 6;

/// The count word in front of a counted object holds the count in its low 62 bits.
const COUNT_MASK: u64 = (1 << 62) - 1;

struct Props {
    /// The environment's own drop entry, put back when the entry goes.
    drop: DropFn,
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
    let entry = *((v[1] + TABLE_DROP) as *const Option<DropFn>);
    if let Some(drop) = entry {
        drop(v[0] as *mut u8);
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
        let props = t.entry(env as usize).or_insert_with(|| {
            // The environment now drops its properties too.
            let slot = env as *mut DropFn;
            let drop = *slot;
            *slot = velt_rt_fnprop_env_drop;
            Props {
                drop,
                entries: vec![],
            }
        });
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

/// The size of an identity environment's block: the count, then the drop, clone and transfer
/// entries of every environment's header.
const IDENTITY_BLOCK: u64 = 32;

/// A new environment holding nothing, counted once: the identity of a function value that had
/// none (a declared function's value, a closure without captures made where functions need no
/// identity), given to it at its first property write.
#[no_mangle]
pub extern "C" fn velt_rt_fnprop_identity() -> *mut u8 {
    let p = crate::mem::velt_rt_alloc(IDENTITY_BLOCK, 8);
    // SAFETY: a fresh block of four words.
    unsafe {
        let w = p as *mut usize;
        w.write(1);
        w.add(1).write(identity_drop as *const () as usize);
        w.add(2).write(identity_clone as *const () as usize);
        w.add(3).write(0);
        p.add(8)
    }
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
        let p = t.remove(&key).map(|p| p.entries);
        *(env as *mut DropFn) = drop;
        (drop, p)
    });
    for (mut name, v) in gone.into_iter().flatten() {
        name.release();
        drop_any(*v);
    }
    drop(env);
}
