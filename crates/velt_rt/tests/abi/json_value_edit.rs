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
fn deleting_from_either_end_stays_linear() {
    // Removing the last member moves nothing, and removing the first leaves a hole that is
    // skipped: `8 * n` deletions take about 8 times the work of `n`, not 64 times. Counted
    // (slots moved or visited, nodes copied on write), not timed, so a busy machine cannot make
    // it flaky.
    for order in [Order::FromTheEnd, Order::FromTheFront] {
        let n = 5_000;
        let (small, large) = (delete_work(n, order), delete_work(8 * n, order));
        assert!(
            large < small * 9,
            "deleting {n} keys took {small} steps, {} took {large}: not linear",
            8 * n
        );
    }
    // The count sees copies on write: deleting from a shared object copies its members once.
    let n = 1_000;
    let mut obj = object(n);
    let shared = unsafe { velt_rt_json_value_clone(obj) };
    let before = work();
    assert_eq!(delete(&mut obj, "k0"), 1);
    let copied = work() - before;
    assert!(
        copied >= n,
        "a delete from a shared object counted {copied} steps"
    );
    for h in [obj, shared] {
        unsafe { velt_rt_json_value_free(h) };
    }
}

#[derive(Clone, Copy)]
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
    let before = work();
    for i in keys {
        assert_eq!(delete(&mut obj, &format!("k{i}")), 1);
    }
    let work = work() - before;
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

/// Edit work counted on this thread so far (see `json::object::WORK`).
fn work() -> usize {
    crate::json::object::WORK.with(|w| w.get())
}

fn at_number(obj: ValueHandle, i: u64) -> Option<f64> {
    let h = unsafe { velt_rt_json_value_at(obj, i) };
    if h.is_null() {
        return None;
    }
    let n = unsafe { velt_rt_json_value_as_f64(h) };
    unsafe { velt_rt_json_value_free(h) };
    Some(n)
}

#[test]
fn deleting_in_bulk_from_any_end_does_linear_work() {
    // Front to back, back to front, and every other key from the middle out: each pass costs
    // O(n) in total (it was O(n) per delete from the front), and order and lookups stay right.
    let n = 20_000;
    let orders: [Vec<usize>; 3] = [
        (0..n).collect(),
        (0..n).rev().collect(),
        (n / 2..n).chain((0..n / 2).rev()).step_by(2).collect(),
    ];
    for order in orders {
        let mut obj = object(n);
        let mut alive = vec![true; n];
        let before = work();
        for (step, &i) in order.iter().enumerate() {
            assert_eq!(delete(&mut obj, &format!("k{i}")), 1);
            alive[i] = false;
            if step % 4_999 == 0 {
                let left: Vec<usize> = (0..n).filter(|&i| alive[i]).collect();
                check_members(obj, &left);
            }
        }
        let left: Vec<usize> = (0..n).filter(|&i| alive[i]).collect();
        check_members(obj, &left);
        let spent = work() - before;
        assert!(
            spent <= 8 * n,
            "{} deletes moved {spent} slots",
            order.len()
        );
        unsafe { velt_rt_json_value_free(obj) };
    }
}

#[test]
fn emptying_from_the_front_by_position_does_linear_work() {
    // `while (v.len() > 0) v.delete(v.keyAt(0))`: positions stay O(1) after front deletes.
    let n = 20_000;
    let mut obj = object(n);
    let before = work();
    for i in 0..n {
        let key = key_at(obj, 0).unwrap();
        assert_eq!(key, format!("k{i}"));
        assert_eq!(at_number(obj, 0), Some(i as f64));
        assert_eq!(delete(&mut obj, &key), 1);
    }
    assert_eq!(unsafe { velt_rt_json_value_len(obj) }, 0);
    let spent = work() - before;
    assert!(spent <= 4 * n, "{n} deletes moved {spent} slots");
    unsafe { velt_rt_json_value_free(obj) };
}

#[test]
fn positions_between_middle_deletes_do_linear_work() {
    // Delete from the middle of the object, reading a position after each delete: every read
    // must not rebuild the positions of all members (n reads of an n-member object would cost
    // n^2), and inserts between them must not either. Counted, so `4 * n` must cost about 4
    // times the work of `n`.
    let spent = |n: usize| {
        let mut obj = object(2 * n);
        let before = work();
        for i in n / 2..n / 2 + n {
            assert_eq!(delete(&mut obj, &format!("k{i}")), 1);
            assert_eq!(key_at(obj, 0).as_deref(), Some("k0"));
            let len = unsafe { velt_rt_json_value_len(obj) };
            assert_eq!(at_number(obj, len - 1), Some((2 * n - 1) as f64));
            if i % 3 == 0 {
                let v = velt_rt_json_value_new_number(i as f64);
                let key = format!("new{i}");
                assert_eq!(
                    unsafe { velt_rt_json_value_set(&mut obj, &borrow(&key), v) },
                    1
                );
                unsafe { velt_rt_json_value_free(v) };
                let len = unsafe { velt_rt_json_value_len(obj) };
                assert_eq!(key_at(obj, len - 1), Some(key.clone()));
                assert_eq!(delete(&mut obj, &key), 1);
            }
        }
        let spent = work() - before;
        assert_eq!(unsafe { velt_rt_json_value_len(obj) }, n as u64);
        unsafe { velt_rt_json_value_free(obj) };
        spent
    };
    let (small, large) = (spent(2_000), spent(8_000));
    assert!(
        large < small * 6,
        "2000 middle deletes cost {small} work, 8000 cost {large}: not linear"
    );
}

#[test]
fn interleaved_edits_match_a_model() {
    // Random set / delete / at / keyAt against a plain vector of (key, value) in order.
    let mut obj = velt_rt_json_value_new_object();
    let mut model: Vec<(usize, f64)> = Vec::new();
    let mut seed = 0x9e37_79b9_u64;
    let mut next = |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    };
    for step in 0..30_000 {
        let k = next(400) as usize;
        let key = format!("k{k}");
        match next(4) {
            0 | 1 => {
                let v = velt_rt_json_value_new_number(step as f64);
                assert_eq!(
                    unsafe { velt_rt_json_value_set(&mut obj, &borrow(&key), v) },
                    1
                );
                unsafe { velt_rt_json_value_free(v) };
                match model.iter_mut().find(|(mk, _)| *mk == k) {
                    Some(e) => e.1 = step as f64,
                    None => model.push((k, step as f64)),
                }
            }
            2 => {
                let had = model.iter().position(|(mk, _)| *mk == k);
                assert_eq!(delete(&mut obj, &key), had.is_some() as u8);
                if let Some(p) = had {
                    model.remove(p);
                }
            }
            _ => {
                let i = next(model.len() as u64 + 1) as usize;
                let want = model.get(i);
                assert_eq!(key_at(obj, i as u64), want.map(|(mk, _)| format!("k{mk}")));
                assert_eq!(at_number(obj, i as u64), want.map(|e| e.1));
            }
        }
        assert_eq!(unsafe { velt_rt_json_value_len(obj) }, model.len() as u64);
    }
    for (k, v) in &model {
        assert_eq!(number_of(obj, &format!("k{k}")), Some(*v));
    }
    unsafe { velt_rt_json_value_free(obj) };
}

#[test]
fn clones_of_objects_with_holes_stay_apart() {
    let n = 100;
    let mut obj = object(n);
    for i in (10..90).step_by(3) {
        assert_eq!(delete(&mut obj, &format!("k{i}")), 1);
    }
    let alive: Vec<usize> = (0..n)
        .filter(|i| !(10..90).contains(i) || (i - 10) % 3 != 0)
        .collect();
    let mut copy = unsafe { velt_rt_json_value_clone(obj) };
    for i in [0, 11, 99] {
        assert_eq!(delete(&mut copy, &format!("k{i}")), 1);
    }
    let v = velt_rt_json_value_new_null();
    assert_eq!(
        unsafe { velt_rt_json_value_set(&mut copy, &borrow("k10"), v) },
        1
    );
    check_members(obj, &alive);
    assert_eq!(
        unsafe { velt_rt_json_value_len(copy) },
        alive.len() as u64 - 2
    );
    assert_eq!(key_at(copy, 0).as_deref(), Some("k1"));
    let mut text = MaybeUninit::<VeltStr>::uninit();
    unsafe { velt_rt_json_value_stringify(copy, text.as_mut_ptr()) };
    let mut text = unsafe { text.assume_init() };
    let s = String::from_utf8(unsafe { text.as_bytes() }.to_vec()).unwrap();
    assert!(
        s.starts_with("{\"k1\":1,") && s.ends_with("\"k98\":98,\"k10\":null}"),
        "{s}"
    );
    unsafe {
        velt_rt_str_drop(&mut text);
        for h in [obj, copy, v] {
            velt_rt_json_value_free(h);
        }
    }
}
