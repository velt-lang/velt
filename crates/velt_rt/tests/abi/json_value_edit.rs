//! Editing `json.Value`s through the C ABI at scale: deleting many members keeps order and
//! lookups right and costs little, and positions beyond `usize` (wasm32) are out of range.

use super::json_reader::borrow;
use crate::json::value_abi::*;
use crate::json::value_edit::*;
use crate::str::{velt_rt_str_drop, VeltStr};
use std::mem::MaybeUninit;

fn object(n: usize) -> ValueHandle {
    let mut obj = velt_rt_json_value_new_object();
    for i in 0..n {
        let k = format!("k{i}");
        unsafe {
            let v = velt_rt_json_value_new_number(i as f64);
            assert_eq!(velt_rt_json_value_set(&mut obj, &borrow(&k), v), 1);
            velt_rt_json_value_free(v);
        }
    }
    obj
}

fn delete(obj: &mut ValueHandle, key: &str) -> u8 {
    unsafe { velt_rt_json_value_delete(obj, &borrow(key)) }
}

fn number_of(obj: ValueHandle, key: &str) -> Option<f64> {
    let h = unsafe { velt_rt_json_value_get(obj, &borrow(key)) };
    if h.is_null() {
        return None;
    }
    let n = unsafe { velt_rt_json_value_as_f64(h) };
    unsafe { velt_rt_json_value_free(h) };
    Some(n)
}

fn key_at(obj: ValueHandle, i: u64) -> Option<String> {
    let mut out = MaybeUninit::<VeltStr>::uninit();
    if unsafe { velt_rt_json_value_key_at(obj, i, out.as_mut_ptr()) } == 0 {
        return None;
    }
    let mut s = unsafe { out.assume_init() };
    let text = String::from_utf8(unsafe { s.as_bytes() }.to_vec()).unwrap();
    unsafe { velt_rt_str_drop(&mut s) };
    Some(text)
}

/// Every member of `obj` is `k<i>: i` for the expected `i`s, in order, and found by key.
fn check_members(obj: ValueHandle, expected: &[usize]) {
    assert_eq!(
        unsafe { velt_rt_json_value_len(obj) },
        expected.len() as u64
    );
    for (pos, &i) in expected.iter().enumerate() {
        let key = format!("k{i}");
        assert_eq!(key_at(obj, pos as u64).as_deref(), Some(key.as_str()));
        assert_eq!(number_of(obj, &key), Some(i as f64), "lookup of {key}");
    }
}

#[test]
fn deleting_many_keys_keeps_order_and_lookups() {
    let n = 300;
    let mut obj = object(n);
    let shared = unsafe { velt_rt_json_value_clone(obj) };
    // Every third key, front to back, then from the back, crossing the index threshold (16).
    let mut alive: Vec<usize> = (0..n).collect();
    for i in (0..n).step_by(3).chain((0..n).rev().filter(|i| i % 3 == 1)) {
        assert_eq!(delete(&mut obj, &format!("k{i}")), 1);
        assert_eq!(delete(&mut obj, &format!("k{i}")), 0);
        alive.retain(|&a| a != i);
        if alive.len().is_multiple_of(37) || alive.len() < 20 {
            check_members(obj, &alive);
        }
    }
    check_members(obj, &alive);
    // Re-adding goes to the end; the clone saw none of it (copy-on-write).
    let v = velt_rt_json_value_new_null();
    assert_eq!(
        unsafe { velt_rt_json_value_set(&mut obj, &borrow("k0"), v) },
        1
    );
    assert_eq!(key_at(obj, alive.len() as u64).as_deref(), Some("k0"));
    check_members(shared, &(0..n).collect::<Vec<_>>());
    for h in [obj, shared, v] {
        unsafe { velt_rt_json_value_free(h) };
    }
}

#[test]
fn deleting_from_the_end_stays_linear() {
    // Removing the last member moves nothing: `8 * n` deletions take about 8 times the work of
    // `n`, not 64 times. Counted (entries searched and moved, index slots adjusted), not timed,
    // so a busy machine cannot make it flaky.
    let n = 5_000;
    let (small, large) = (
        delete_work(n, Order::FromTheEnd),
        delete_work(8 * n, Order::FromTheEnd),
    );
    assert!(
        large < small * 9,
        "deleting {n} keys took {small} steps, {} took {large}: not linear",
        8 * n
    );
    // The count sees the moves: from the front, every delete moves all the members after it.
    let n = 1_000;
    let front = delete_work(n, Order::FromTheFront);
    assert!(
        front > n * n / 4,
        "deleting {n} keys from the front counted {front} steps"
    );
}

enum Order {
    FromTheEnd,
    FromTheFront,
}

/// The steps deleting every member of an `n`-member object one by one takes.
fn delete_work(n: usize, order: Order) -> usize {
    let mut obj = object(n);
    let keys: Vec<usize> = match order {
        Order::FromTheEnd => (0..n).rev().collect(),
        Order::FromTheFront => (0..n).collect(),
    };
    let before = crate::json::value::remove_work();
    for i in keys {
        assert_eq!(delete(&mut obj, &format!("k{i}")), 1);
    }
    let work = crate::json::value::remove_work() - before;
    assert_eq!(unsafe { velt_rt_json_value_len(obj) }, 0);
    unsafe { velt_rt_json_value_free(obj) };
    work
}

#[test]
fn positions_beyond_usize_are_out_of_range() {
    // On wasm32 `usize` is `u32`: a cast turned position 2^32 into 0. The helper is generic, so
    // the 32-bit case is checked here with `u32` standing in for wasm32's `usize`.
    assert_eq!(to_index::<u32>(4_294_967_295), Some(u32::MAX));
    assert_eq!(to_index::<u32>(4_294_967_296), None);
    assert_eq!(to_index::<u32>(u64::MAX), None);
    assert_eq!(to_index::<usize>(7), Some(7));
    // Through the ABI (on 64-bit hosts these are merely large): nothing is read or overwritten.
    unsafe {
        let mut arr = velt_rt_json_value_new_array();
        let one = velt_rt_json_value_new_number(1.0);
        let two = velt_rt_json_value_new_number(2.0);
        assert_eq!(velt_rt_json_value_push(&mut arr, one), 1);
        for i in [1u64 << 32, u64::MAX] {
            assert_eq!(velt_rt_json_value_set_at(&mut arr, i, two), 0);
            assert!(velt_rt_json_value_at(arr, i).is_null());
        }
        let obj = object(1);
        assert_eq!(key_at(obj, 1 << 32), None);
        assert!(velt_rt_json_value_at(obj, 1 << 32).is_null());
        let first = velt_rt_json_value_at(arr, 0);
        assert_eq!(velt_rt_json_value_as_f64(first), 1.0);
        for h in [arr, one, two, obj, first] {
            velt_rt_json_value_free(h);
        }
    }
}
