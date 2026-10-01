//! Named placeholders: `WHERE id = :id AND org = $org` becomes `WHERE id = $1 AND org = $2`
//! with the names `["id", "org"]`, so an object of parameters binds by field name while the
//! server only ever sees PostgreSQL's positional `$n`.
//!
//! The scanner knows enough SQL to leave everything that only looks like a placeholder alone:
//! `::type` casts, string literals (`'…'`, `E'…\'…'`), quoted identifiers (`"…"`), dollar-quoted
//! strings (`$$…$$`, `$tag$…$tag$`), comments (`-- …`, nested `/* … */`) and `$` inside
//! identifiers (`a$b`). A repeated name reuses its number. Mixing named placeholders with
//! positional `$1` is an error. An array slice with a named bound needs spaces (`a[lo : hi]`).

/// SQL rewritten to positional placeholders, and the parameter name of each `$n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewritten {
    /// The SQL with `$1..$n`.
    pub sql: String,
    /// `names[i]` is bound to `$(i+1)`.
    pub names: Vec<String>,
}

fn ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn ident_char(b: u8) -> bool {
    ident_start(b) || b.is_ascii_digit() || b == b'$'
}

struct Scan<'a> {
    src: &'a [u8],
    pos: usize,
    out: Vec<u8>,
    names: Vec<String>,
}

impl Scan<'_> {
    /// Copy `src[pos..end]` to the output and move past it.
    fn copy_to(&mut self, end: usize) {
        let end = end.min(self.src.len());
        self.out.extend_from_slice(&self.src[self.pos..end]);
        self.pos = end;
    }

    fn find(&self, from: usize, needle: &[u8]) -> usize {
        self.src[from.min(self.src.len())..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map_or(self.src.len(), |i| from + i + needle.len())
    }

    /// End of a quoted run starting at `pos` (`'…'` or `"…"`; a doubled quote continues it;
    /// `backslash` for `E'…'` strings).
    fn quoted_end(&self, quote: u8, backslash: bool) -> usize {
        let mut i = self.pos + 1;
        while i < self.src.len() {
            let b = self.src[i];
            if backslash && b == b'\\' {
                i += 2;
            } else if b == quote {
                if self.src.get(i + 1) == Some(&quote) {
                    i += 2;
                } else {
                    return i + 1;
                }
            } else {
                i += 1;
            }
        }
        self.src.len()
    }

    /// End of a (nested) block comment starting at `pos`.
    fn block_comment_end(&self) -> usize {
        let (mut i, mut depth) = (self.pos + 2, 1);
        while i + 1 < self.src.len() {
            match (self.src[i], self.src[i + 1]) {
                (b'/', b'*') => (depth, i) = (depth + 1, i + 2),
                (b'*', b'/') => {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => i += 1,
            }
        }
        self.src.len()
    }

    fn ident_end(&self, from: usize) -> usize {
        let mut i = from;
        while i < self.src.len() && ident_char(self.src[i]) && self.src[i] != b'$' {
            i += 1;
        }
        i
    }

    /// Replace the placeholder name `src[start..end]` (its sigil at `pos`) with `$n`.
    fn placeholder(&mut self, start: usize, end: usize) {
        // Names are ASCII identifier bytes or whole UTF-8 sequences (bytes >= 0x80).
        let name = String::from_utf8_lossy(&self.src[start..end]).into_owned();
        let n = match self.names.iter().position(|x| *x == name) {
            Some(i) => i + 1,
            None => {
                self.names.push(name);
                self.names.len()
            }
        };
        self.out.push(b'$');
        crate::fmt::push_u64(&mut self.out, n as u64);
        self.pos = end;
    }

    /// Handle a `$` at `pos`: positional parameter, dollar quote or named placeholder.
    fn dollar(&mut self) -> Result<(), String> {
        let next = self.src.get(self.pos + 1).copied().unwrap_or(0);
        if next.is_ascii_digit() {
            return Err(
                "positional placeholders ($1) cannot be mixed with named parameters; \
                 pass an array to bind $1, $2, …"
                    .to_string(),
            );
        }
        if next == b'$' {
            let end = self.find(self.pos + 2, b"$$");
            self.copy_to(end);
            return Ok(());
        }
        if !ident_start(next) {
            self.copy_to(self.pos + 1);
            return Ok(());
        }
        let end = self.ident_end(self.pos + 1);
        if self.src.get(end) == Some(&b'$') {
            let tag = self.src[self.pos..=end].to_vec();
            let close = self.find(end + 1, &tag);
            self.copy_to(close);
        } else {
            self.placeholder(self.pos + 1, end);
        }
        Ok(())
    }

    /// Handle a `:` at `pos`: a `::` cast or a named placeholder.
    fn colon(&mut self) {
        let next = self.src.get(self.pos + 1).copied().unwrap_or(0);
        if next == b':' {
            self.copy_to(self.pos + 2);
        } else if ident_start(next) {
            let end = self.ident_end(self.pos + 1);
            self.placeholder(self.pos + 1, end);
        } else {
            self.copy_to(self.pos + 1);
        }
    }

    fn step(&mut self) -> Result<(), String> {
        let b = self.src[self.pos];
        let next = self.src.get(self.pos + 1).copied();
        match b {
            b'\'' => {
                let prev = self.pos.checked_sub(1).map(|i| self.src[i]);
                let before = self.pos.checked_sub(2).map(|i| self.src[i]);
                let escaped = matches!(prev, Some(b'E' | b'e')) && !before.is_some_and(ident_char);
                self.copy_to(self.quoted_end(b'\'', escaped));
            }
            b'"' => self.copy_to(self.quoted_end(b'"', false)),
            b'-' if next == Some(b'-') => self.copy_to(self.find(self.pos, b"\n")),
            b'/' if next == Some(b'*') => self.copy_to(self.block_comment_end()),
            b'$' => self.dollar()?,
            b':' => self.colon(),
            _ if ident_start(b) => {
                let mut end = self.pos + 1;
                while end < self.src.len() && ident_char(self.src[end]) {
                    end += 1;
                }
                self.copy_to(end);
            }
            _ => self.copy_to(self.pos + 1),
        }
        Ok(())
    }
}

/// Rewrite `sql`'s named placeholders (see the module docs).
pub fn rewrite_named(sql: &str) -> Result<Rewritten, String> {
    let mut scan = Scan {
        src: sql.as_bytes(),
        pos: 0,
        out: Vec::with_capacity(sql.len()),
        names: Vec::new(),
    };
    while scan.pos < scan.src.len() {
        scan.step()?;
    }
    Ok(Rewritten {
        // Only ASCII was inserted and every copied run ends on a character boundary.
        sql: String::from_utf8_lossy(&scan.out).into_owned(),
        names: scan.names,
    })
}
