//! RFC 6455 framing: parsing (and unmasking in place) and encoding.

/// Frame opcodes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Op {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl Op {
    fn code(self) -> u8 {
        match self {
            Op::Continuation => 0,
            Op::Text => 1,
            Op::Binary => 2,
            Op::Close => 8,
            Op::Ping => 9,
            Op::Pong => 10,
        }
    }

    fn from_code(c: u8) -> Option<Op> {
        Some(match c {
            0 => Op::Continuation,
            1 => Op::Text,
            2 => Op::Binary,
            8 => Op::Close,
            9 => Op::Ping,
            10 => Op::Pong,
            _ => return None,
        })
    }

    fn is_control(self) -> bool {
        matches!(self, Op::Close | Op::Ping | Op::Pong)
    }
}

/// A whole frame at the start of a buffer: the payload is `buf[start..start + len]` (unmasked),
/// and the frame takes `consumed` bytes.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Frame {
    pub fin: bool,
    pub op: Op,
    pub start: usize,
    pub len: usize,
    pub consumed: usize,
}

/// The frame at the start of `buf`, unmasking its payload in place; `Ok(None)` if `buf` holds
/// only part of it. `must_mask`: a server rejects unmasked frames (RFC 6455 §5.1).
pub(super) fn parse(
    buf: &mut [u8],
    must_mask: bool,
    max: usize,
) -> Result<Option<Frame>, &'static str> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let (b0, b1) = (buf[0], buf[1]);
    if b0 & 0x70 != 0 {
        return Err("reserved bits set (no extension was negotiated)");
    }
    let op = Op::from_code(b0 & 0x0f).ok_or("unknown opcode")?;
    let fin = b0 & 0x80 != 0;
    let masked = b1 & 0x80 != 0;
    if must_mask && !masked {
        return Err("unmasked frame from a client");
    }
    let mut at = 2;
    let len = match b1 & 0x7f {
        126 => {
            if buf.len() < 4 {
                return Ok(None);
            }
            at = 4;
            u16::from_be_bytes([buf[2], buf[3]]) as u64
        }
        127 => {
            if buf.len() < 10 {
                return Ok(None);
            }
            at = 10;
            u64::from_be_bytes(buf[2..10].try_into().unwrap())
        }
        n => n as u64,
    };
    if op.is_control() && (len > 125 || !fin) {
        return Err("invalid control frame");
    }
    if len > max as u64 {
        return Err("message too long");
    }
    let len = len as usize;
    let mask = if masked {
        if buf.len() < at + 4 {
            return Ok(None);
        }
        let m = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
        at += 4;
        Some(m)
    } else {
        None
    };
    if buf.len() < at + len {
        return Ok(None);
    }
    if let Some(m) = mask {
        unmask(&mut buf[at..at + len], m);
    }
    Ok(Some(Frame {
        fin,
        op,
        start: at,
        len,
        consumed: at + len,
    }))
}

/// XORs `data` with the repeating 4-byte `mask`, eight bytes at a time.
fn unmask(data: &mut [u8], m: [u8; 4]) {
    let wide = u64::from_ne_bytes([m[0], m[1], m[2], m[3], m[0], m[1], m[2], m[3]]);
    let (chunks, rest) = data.as_chunks_mut::<8>();
    for c in chunks {
        *c = (u64::from_ne_bytes(*c) ^ wide).to_ne_bytes();
    }
    for (i, b) in rest.iter_mut().enumerate() {
        *b ^= m[i & 3];
    }
}

/// Appends a frame (FIN set) carrying `payload` to `out`; a client masks it.
pub(super) fn encode(out: &mut Vec<u8>, op: Op, payload: &[u8], mask: bool) {
    let len = payload.len();
    let mask_bit = if mask { 0x80 } else { 0 };
    out.reserve(14 + len);
    out.push(0x80 | op.code());
    if len < 126 {
        out.push(mask_bit | len as u8);
    } else if len < 65536 {
        out.push(mask_bit | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(mask_bit | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    if mask {
        let m = mask_key();
        out.extend_from_slice(&m);
        let at = out.len();
        out.extend_from_slice(payload);
        unmask(&mut out[at..], m);
    } else {
        out.extend_from_slice(payload);
    }
}

/// A fresh masking key (RFC 6455 §5.3: unpredictable; a per-thread xorshift seeded from the OS).
fn mask_key() -> [u8; 4] {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = Cell::new({
            let mut seed = [0u8; 8];
            let _ = getrandom::fill(&mut seed);
            u64::from_ne_bytes(seed) | 1
        });
    }
    STATE.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x as u32).to_ne_bytes()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked(op: Op, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        encode(&mut v, op, payload, true);
        v
    }

    #[test]
    fn round_trips_every_length_class() {
        for n in [0usize, 1, 7, 8, 125, 126, 127, 65535, 65536, 70000] {
            let payload: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            let mut buf = masked(Op::Binary, &payload);
            let f = parse(&mut buf, true, 1 << 20).unwrap().unwrap();
            assert_eq!(
                (f.fin, f.op, f.len, f.consumed),
                (true, Op::Binary, n, buf.len())
            );
            assert_eq!(&buf[f.start..f.start + f.len], &payload[..]);
        }
    }

    #[test]
    fn partial_frames_need_more_bytes() {
        let buf = masked(Op::Text, b"hello world");
        for cut in 0..buf.len() {
            let mut part = buf[..cut].to_vec();
            assert_eq!(parse(&mut part, true, 1 << 20), Ok(None), "cut at {cut}");
        }
    }

    #[test]
    fn server_rules() {
        let mut unmasked = Vec::new();
        encode(&mut unmasked, Op::Text, b"hi", false);
        assert!(parse(&mut unmasked.clone(), true, 1 << 20).is_err());
        assert!(parse(&mut unmasked, false, 1 << 20).unwrap().is_some());
        let mut big_ping = masked(Op::Ping, &[0; 126]);
        assert!(parse(&mut big_ping, true, 1 << 20).is_err());
        let mut too_long = masked(Op::Binary, &[0; 100]);
        assert!(parse(&mut too_long, true, 99).is_err());
        let mut reserved = masked(Op::Text, b"x");
        reserved[0] |= 0x40;
        assert!(parse(&mut reserved, true, 1 << 20).is_err());
    }
}
