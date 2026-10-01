//! Binary `inet`, `cidr` and `macaddr` values as the text PostgreSQL prints for them.
//!
//! `inet` / `cidr`: family (2 = IPv4, 3 = IPv6), prefix bits, an is-cidr flag, the address
//! length, then the address. An `inet` whose prefix covers the whole address prints without it
//! (`"10.0.0.1"`, `"10.0.0.0/8"`); a `cidr` always has it. IPv6 addresses are compressed as
//! PostgreSQL and RFC 5952 do (`"2001:db8::1"`). `macaddr`: six bytes, `"08:00:2b:01:02:03"`.

use std::fmt::Write;
use std::net::{Ipv4Addr, Ipv6Addr};

/// `inet` or `cidr`.
pub fn inet(raw: &[u8]) -> Result<String, String> {
    let bad = || "bad inet value".to_string();
    let [family, bits, is_cidr, len, addr @ ..] = raw else {
        return Err(bad());
    };
    let (text, max_bits) = match (family, len, addr.len()) {
        (2, 4, 4) => (
            Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]).to_string(),
            32,
        ),
        (3, 16, 16) => {
            let b: [u8; 16] = addr.try_into().map_err(|_| bad())?;
            (Ipv6Addr::from(b).to_string(), 128)
        }
        _ => return Err(bad()),
    };
    Ok(if *is_cidr == 0 && *bits == max_bits {
        text
    } else {
        format!("{text}/{bits}")
    })
}

/// `macaddr` (6 bytes) or `macaddr8` (8 bytes).
pub fn macaddr(raw: &[u8]) -> Result<String, String> {
    if raw.len() != 6 && raw.len() != 8 {
        return Err("bad macaddr value".to_string());
    }
    let mut s = String::with_capacity(raw.len() * 3);
    for (i, b) in raw.iter().enumerate() {
        if i > 0 {
            s.push(':');
        }
        let _ = write!(s, "{b:02x}");
    }
    Ok(s)
}
