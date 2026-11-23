//! The TLS server's and client's rows of the hook table (#2392): std/net/tls/fd_server.velt and
//! fd_client.velt, which `listenTls`, `connectTls` and `fetch`'s `https:` call by role, so a
//! program loads them only where live code reaches them. They can't be served in the middle of a
//! check (velt_loader's (a1) rule), so a demand of one is refused and the program is checked again
//! with it up front (veltc `sema_stage`). Kept apart from the rest of the table ([`super`]), which
//! [`with_tls_hooks`] gives on, with these rows after it, to `with_decorator_hooks!`.

/// `with_decorator_hooks!` over the rows given, then the TLS server's and client's.
macro_rules! with_tls_hooks {
    ($($rows:tt)*) => {
        with_decorator_hooks! {
            $($rows)*
            sched_tls_listen => net_tls_fd_server, "velt_rt_tls_listen", sig(async (move string, move string, move string, move string, u64) -> IoResult(u64));
            sched_tls_connect => net_tls_fd_client, "velt_rt_tls_connect", sig(async (move string, move string, move string, move string) -> IoResult(u64));
            sched_tls_handshake => net_tls_fd_client, "velt_rt_tls_handshake", sig(async (u64, move string, move string, move string, i64, move or_null(AbortSignal)) -> IoResult(u64));
        }
    };
}
