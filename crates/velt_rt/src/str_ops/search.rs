//! Comparing strings: `==` and well-formedness. The searches (`indexOf`, `lastIndexOf`,
//! `includes`, `startsWith`, `endsWith`) are Velt code (std/lang/string/search.velt, #2198), and so
//! is their position translation (std/lang/string/positions.velt).

use crate::str::VeltStr;

/// `a == b` on strings: length check, then one `memcmp`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_eq(a: *const VeltStr, b: *const VeltStr) -> u8 {
    let (a, b) = (&*a, &*b);
    (a.as_bytes() == b.as_bytes()) as u8
}
