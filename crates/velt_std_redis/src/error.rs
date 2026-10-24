//! Errors of Redis operations and their mapping to Velt's `IoError` (rt_abi_async.md §3, §14.12).
//!
//! I/O failures keep the shared I/O codes (`ECONNREFUSED`, `ETIMEDOUT`…). An error *reply* from
//! the server gets its own code, [`SERVER_ERROR`], with the reply text as the message
//! (`WRONGTYPE Operation against a key…`): std/redis takes the first word as `RedisError.code`.

use std::io;
use velt_native::code;

/// `VeltErr::code` of an error reply from the server (`-ERR …`, `-WRONGTYPE …`).
pub const SERVER_ERROR: i32 = 100;

/// A failed operation, cheap to clone (one failure fails every command waiting on the connection).
#[derive(Debug, Clone, PartialEq)]
pub struct RedisErr {
    /// A `result::code` value or [`SERVER_ERROR`].
    pub code: i32,
    /// Human-readable message (the reply text for server errors).
    pub message: String,
}

impl RedisErr {
    /// An I/O failure.
    pub fn io(e: &io::Error) -> RedisErr {
        RedisErr {
            code: code::of(e),
            message: format!("Redis connection failed: {e}"),
        }
    }

    /// An error reply (`-CODE message`).
    pub fn server(reply: String) -> RedisErr {
        RedisErr {
            code: SERVER_ERROR,
            message: reply,
        }
    }

    /// A bad argument (URL, TLS configuration…).
    pub fn invalid(message: String) -> RedisErr {
        RedisErr {
            code: code::INVALID_INPUT,
            message,
        }
    }

    /// A reply that does not follow RESP.
    pub fn protocol(message: String) -> RedisErr {
        RedisErr {
            code: code::INVALID_DATA,
            message: format!("Redis protocol error: {message}"),
        }
    }

    /// The connection is gone (closed by the server, or failed earlier).
    pub fn closed() -> RedisErr {
        RedisErr {
            code: code::CONNECTION_RESET,
            message: "the Redis connection is closed".to_string(),
        }
    }

    /// This error as Velt receives it.
    pub fn to_error(&self) -> velt_native::Error {
        velt_native::Error::new(self.code, self.message.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_codes() {
        let e = RedisErr::io(&io::Error::from(io::ErrorKind::ConnectionRefused));
        assert_eq!(e.code, code::CONNECTION_REFUSED);
        assert!(e.message.starts_with("Redis connection failed"));
        let e = RedisErr::server("WRONGTYPE bad".into());
        assert_eq!(
            (e.code, e.message.as_str()),
            (SERVER_ERROR, "WRONGTYPE bad")
        );
    }
}
