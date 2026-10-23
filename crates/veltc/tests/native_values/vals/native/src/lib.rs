//! Arrays and structs across the native boundary (crates/veltc/tests/native_values.rs).

use std::time::Duration;

use velt_native::{export, Array, Error, Str, Value};

velt_native::package!(vals);

#[derive(Value, Clone)]
#[repr(C)]
pub struct Row {
    id: i64,
    name: Str,
    tags: Array<Str>,
}

#[derive(Value)]
#[repr(C)]
pub struct Stats {
    count: i64,
    sum: f64,
}

#[export]
fn velt_vals__reverse(rows: &[Row]) -> Vec<Row> {
    rows.iter().rev().cloned().collect()
}

#[export]
fn velt_vals__stats(xs: &[f64]) -> Stats {
    Stats {
        count: xs.len() as i64,
        sum: xs.iter().sum(),
    }
}

#[export]
fn velt_vals__count(groups: &[Array<Row>]) -> i64 {
    groups.iter().map(|g| g.len() as i64).sum()
}

/// `n` rows named after `names` in turn, after a short sleep on the runtime's timer.
#[export(async)]
async fn velt_vals__later(n: i64, names: Vec<Str>) -> Result<Vec<Row>, Error> {
    velt_native::time::sleep(Duration::from_millis(2)).await;
    if names.is_empty() {
        return Err(Error::other("no names"));
    }
    Ok((0..n)
        .map(|i| {
            let name = names[i as usize % names.len()].clone();
            let tags: Array<Str> = (0..i).map(|k| Str::new(&format!("t{k}"))).collect();
            Row { id: i, name, tags }
        })
        .collect())
}
