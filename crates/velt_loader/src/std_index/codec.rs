//! The std index's binary form ([`super::FORMAT`]): a package's entry is [`FORMAT`], then the
//! package in field order: integers as LEB128 varints, strings as their length and bytes, a
//! `Vec` as its length and elements, an enum as a tag byte and its fields. Decoding never
//! panics: a value of another format, cut short, with trailing bytes or a bad tag is `None`.
//! Integrity is the build cache's job (its entries carry a CRC-32), as for `std-ast` entries.

use super::{Export, ExportKind, ExtendMember, ModuleIndex, PackageIndex, Violation, FORMAT};

/// `package` in the binary form.
pub(crate) fn encode(package: &PackageIndex) -> Vec<u8> {
    let mut w = Vec::with_capacity(1024);
    w.extend_from_slice(FORMAT);
    put_str(&mut w, &package.path);
    put_len(&mut w, package.modules.len());
    for m in &package.modules {
        put_module(&mut w, m);
    }
    w
}

/// The package [`encode`] encoded; `None` when `bytes` is not a whole encoding of [`FORMAT`].
pub(crate) fn decode(bytes: &[u8]) -> Option<PackageIndex> {
    let mut r = Reader {
        bytes: bytes.strip_prefix(FORMAT.as_slice())?,
    };
    let path = r.str()?;
    let modules = r.list(Reader::module)?;
    r.bytes.is_empty().then_some(PackageIndex { path, modules })
}

fn put_module(w: &mut Vec<u8>, m: &ModuleIndex) {
    put_str(w, &m.path);
    put_len(w, m.hooks.len());
    for h in &m.hooks {
        put_str(w, h);
    }
    put_len(w, m.extends.len());
    for e in &m.extends {
        put_str(w, &e.ty);
        put_str(w, &e.member);
        w.push(u8::from(e.is_static));
    }
    put_len(w, m.exports.len());
    for e in &m.exports {
        put_str(w, &e.name);
        w.push(e.kind as u8);
        match &e.from {
            Some(from) => {
                w.push(1);
                put_str(w, from);
            }
            None => w.push(0),
        }
    }
    put_len(w, m.imports.len());
    for i in &m.imports {
        put_str(w, i);
    }
    put_len(w, m.violations.len());
    for v in &m.violations {
        match v {
            Violation::TopLevelStatements => w.push(0),
            Violation::ExtendsForeignType { ty } => {
                w.push(1);
                put_str(w, ty);
            }
            Violation::SubclassesForeignClass { class, base } => {
                w.push(2);
                put_str(w, class);
                put_str(w, base);
            }
            Violation::MergesInterface { name } => {
                w.push(3);
                put_str(w, name);
            }
            Violation::AugmentsGlobal => w.push(4),
            Violation::AugmentsModule { spec } => {
                w.push(5);
                put_str(w, spec);
            }
        }
    }
}

fn put_len(w: &mut Vec<u8>, mut n: usize) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            w.push(b);
            return;
        }
        w.push(b | 0x80);
    }
}

fn put_str(w: &mut Vec<u8>, s: &str) {
    put_len(w, s.len());
    w.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (&b, rest) = self.bytes.split_first()?;
        self.bytes = rest;
        Some(b)
    }

    fn len(&mut self) -> Option<usize> {
        let mut n: u64 = 0;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            n |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                // No length is longer than what is left: a damaged one fails here, before a
                // list allocates for it.
                let n = usize::try_from(n).ok()?;
                return (n <= self.bytes.len()).then_some(n);
            }
        }
        None
    }

    fn str(&mut self) -> Option<String> {
        let n = self.len()?;
        let (s, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        String::from_utf8(s.to_vec()).ok()
    }

    fn bool(&mut self) -> Option<bool> {
        match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn list<T>(&mut self, mut item: impl FnMut(&mut Self) -> Option<T>) -> Option<Vec<T>> {
        let n = self.len()?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(item(self)?);
        }
        Some(out)
    }

    fn module(&mut self) -> Option<ModuleIndex> {
        Some(ModuleIndex {
            path: self.str()?,
            hooks: self.list(Reader::str)?,
            extends: self.list(|r| {
                Some(ExtendMember {
                    ty: r.str()?,
                    member: r.str()?,
                    is_static: r.bool()?,
                })
            })?,
            exports: self.list(|r| {
                Some(Export {
                    name: r.str()?,
                    kind: ExportKind::from_u8(r.byte()?)?,
                    from: match r.bool()? {
                        true => Some(r.str()?),
                        false => None,
                    },
                })
            })?,
            imports: self.list(Reader::str)?,
            violations: self.list(Reader::violation)?,
        })
    }

    fn violation(&mut self) -> Option<Violation> {
        Some(match self.byte()? {
            0 => Violation::TopLevelStatements,
            1 => Violation::ExtendsForeignType { ty: self.str()? },
            2 => Violation::SubclassesForeignClass {
                class: self.str()?,
                base: self.str()?,
            },
            3 => Violation::MergesInterface { name: self.str()? },
            4 => Violation::AugmentsGlobal,
            5 => Violation::AugmentsModule { spec: self.str()? },
            _ => return None,
        })
    }
}
