//! The `counter` package's native library: state that lives in the library, and a failure that
//! crosses into Velt as an `IoError`.

use std::sync::atomic::{AtomicI64, Ordering};

use velt_native::{export, Error};

velt_native::package!(counter);

static COUNT: AtomicI64 = AtomicI64::new(0);

#[export]
fn velt_counter__add(by: i64) -> Result<i64, Error> {
    if by < 0 {
        return Err(Error::new(velt_native::code::INVALID_INPUT, "a negative amount"));
    }
    Ok(COUNT.fetch_add(by, Ordering::Relaxed) + by)
}
