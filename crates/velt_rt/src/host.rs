//! `HostRef` (std/host/host_ref.vlt; https://veltlang.com/docs/reference/host-functions/) natively: a native
//! program has no JS host, so no host function can make one (calling one is a compile error),
//! and these functions only exist so that code dropping or cloning a `HostRef` links. The
//! WebAssembly runtime's own `host.rs` reaches the JS glue's table of values.

/// Release slot `handle` of the host's table: there is none.
#[no_mangle]
pub extern "C" fn velt_rt_host_release(_handle: u32) {}

/// Copy slot `handle` of the host's table: there is none, and no `HostRef` holds a slot.
#[no_mangle]
pub extern "C" fn velt_rt_host_clone(_handle: u32) -> u32 {
    0
}
