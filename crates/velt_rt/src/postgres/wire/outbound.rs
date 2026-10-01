//! Message boundaries in what the driver writes (frontend messages), so the wire knows how many
//! `ReadyForQuery` replies the server owes the driver and where a batch may be slipped in.
//!
//! Frontend messages are `tag, i32 length, body`, except the first ones of a connection:
//! `SSLRequest` and `StartupMessage` have no tag (`i32 length, i32 code, body`). Every `Sync` and
//! every simple `Query` makes the server answer with exactly one `ReadyForQuery` (a `Sync` the
//! server receives during `COPY FROM STDIN` is the exception; the wire corrects for it when it
//! sees the server's `CopyInResponse`).

/// `StartupMessage` codes carry the protocol major version (3) in their high 16 bits; the other
/// untagged messages (`SSLRequest` 80877103, `GSSENCRequest` 80877104) use 1234 there.
const PROTOCOL_MAJOR: u32 = 3;

/// What a run of bytes contained (see [`Outbound::advance`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Sent {
    /// `Sync` messages started.
    pub syncs: u32,
    /// Simple `Query` messages started.
    pub queries: u32,
    /// `SSLRequest`-like messages, each answered by one untagged byte.
    pub negotiations: u32,
}

/// The frontend stream's position (see the module docs).
#[derive(Debug, Clone, Copy)]
pub struct Outbound {
    header: [u8; 8],
    /// Header bytes of the current message seen so far.
    have: usize,
    /// Body bytes of the current message still to come.
    remaining: usize,
    /// Past the `StartupMessage`: every message is tagged.
    tagged: bool,
    /// The last complete message ended a request (`Sync` / `Query`), or the connection has just
    /// started up: the server owes nothing for messages sent so far.
    closed: bool,
}

impl Outbound {
    /// The position at the start of a connection.
    pub fn new() -> Outbound {
        Outbound {
            header: [0; 8],
            have: 0,
            remaining: 0,
            tagged: false,
            closed: false,
        }
    }

    /// Whether a new request may start here: between messages, after one that ended a
    /// request.
    pub fn at_request_end(&self) -> bool {
        self.have == 0 && self.remaining == 0 && self.closed
    }

    /// The startup handshake is over (the server sent its first `ReadyForQuery`).
    pub fn startup_done(&mut self) {
        self.closed = true;
    }

    fn header_len(&self) -> usize {
        if self.tagged {
            5
        } else {
            8
        }
    }

    /// Account for `bytes` written. With `stop_at_request_end`, stops right after the first
    /// message that ends a request; returns the bytes consumed.
    pub fn advance(&mut self, bytes: &[u8], stop_at_request_end: bool, sent: &mut Sent) -> usize {
        let mut at = 0;
        while at < bytes.len() {
            if self.remaining > 0 {
                let n = self.remaining.min(bytes.len() - at);
                self.remaining -= n;
                at += n;
            } else {
                let n = (self.header_len() - self.have).min(bytes.len() - at);
                self.header[self.have..self.have + n].copy_from_slice(&bytes[at..at + n]);
                self.have += n;
                at += n;
                if self.have == self.header_len() {
                    self.start_message(sent);
                }
            }
            if stop_at_request_end && self.at_request_end() {
                break;
            }
        }
        at
    }

    /// A message header is complete: note what it starts.
    fn start_message(&mut self, sent: &mut Sent) {
        let h = self.header;
        self.have = 0;
        if self.tagged {
            let len = u32::from_be_bytes([h[1], h[2], h[3], h[4]]) as usize;
            self.remaining = len.saturating_sub(4);
            self.closed = matches!(h[0], b'S' | b'Q');
            match h[0] {
                b'S' => sent.syncs += 1,
                b'Q' => sent.queries += 1,
                _ => {}
            }
        } else {
            let len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
            let code = u32::from_be_bytes([h[4], h[5], h[6], h[7]]);
            self.remaining = len.saturating_sub(8);
            if code >> 16 == PROTOCOL_MAJOR {
                self.tagged = true;
            } else {
                sent.negotiations += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tagged(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut m = vec![tag];
        m.extend_from_slice(&(body.len() as u32 + 4).to_be_bytes());
        m.extend_from_slice(body);
        m
    }

    fn untagged(code: u32, body: &[u8]) -> Vec<u8> {
        let mut m = (body.len() as u32 + 8).to_be_bytes().to_vec();
        m.extend_from_slice(&code.to_be_bytes());
        m.extend_from_slice(body);
        m
    }

    #[test]
    fn startup_then_requests() {
        let mut out = Outbound::new();
        let mut sent = Sent::default();
        let mut bytes = untagged(80877103, b"");
        bytes.extend(untagged(196608, b"user\0me\0\0"));
        bytes.extend(tagged(b'p', b"secret\0"));
        assert_eq!(out.advance(&bytes, false, &mut sent), bytes.len());
        assert_eq!(sent.negotiations, 1);
        assert!(!out.at_request_end());
        out.startup_done();
        assert!(out.at_request_end());

        let mut request = tagged(b'B', b"\0s1\0\0\0\0\0\0\0");
        request.extend(tagged(b'E', b"\0\0\0\0\0"));
        request.extend(tagged(b'S', b""));
        request.extend(tagged(b'Q', b"SELECT 1\0"));
        // Byte by byte: headers split across writes.
        let mut sent = Sent::default();
        for b in &request {
            out.advance(std::slice::from_ref(b), false, &mut sent);
        }
        assert_eq!((sent.syncs, sent.queries), (1, 1));
        assert!(out.at_request_end());
    }

    #[test]
    fn stops_after_the_first_request() {
        let mut out = Outbound::new();
        let mut sent = Sent::default();
        out.advance(&untagged(196608, b"\0"), false, &mut sent);
        out.startup_done();
        let mut bytes = tagged(b'B', b"xyz");
        let first = bytes.len() + 5;
        bytes.extend(tagged(b'S', b""));
        bytes.extend(tagged(b'B', b"xyz"));
        let mut probe = out;
        assert_eq!(probe.advance(&bytes, true, &mut Sent::default()), first);
        // Mid-request, nothing ends here.
        let mut probe = out;
        assert_eq!(probe.advance(&bytes[..3], true, &mut Sent::default()), 3);
        assert!(!probe.at_request_end());
    }
}
