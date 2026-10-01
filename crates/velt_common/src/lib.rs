//! Shared primitives used by every compiler stage: source files, spans, diagnostics.
//! CONTRACT FILE — changes need maintainer review (docs/internals/contracts/README.md).

use std::fmt;
use std::path::PathBuf;

/// Index into a [`SourceMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct FileId(pub u32);

/// Byte range `lo..hi` inside one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub file: FileId,
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub const DUMMY: Span = Span {
        file: FileId(0),
        lo: 0,
        hi: 0,
    };

    pub fn new(file: FileId, lo: u32, hi: u32) -> Self {
        Span { file, lo, hi }
    }

    /// Smallest span covering both (must be in the same file).
    pub fn to(self, other: Span) -> Span {
        Span {
            file: self.file,
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }
}

pub struct SourceFile {
    pub path: PathBuf,
    pub src: String,
}

#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, path: impl Into<PathBuf>, src: impl Into<String>) -> FileId {
        self.files.push(SourceFile {
            path: path.into(),
            src: src.into(),
        });
        FileId(self.files.len() as u32 - 1)
    }

    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn files(&self) -> impl Iterator<Item = (FileId, &SourceFile)> {
        self.files
            .iter()
            .enumerate()
            .map(|(i, f)| (FileId(i as u32), f))
    }

    /// 1-based (line, column) for a byte offset.
    pub fn line_col(&self, id: FileId, offset: u32) -> (usize, usize) {
        let src = &self.get(id).src;
        let off = (offset as usize).min(src.len());
        let before = &src[..off];
        let line = before.matches('\n').count() + 1;
        let col = before.rfind('\n').map_or(off, |nl| off - nl - 1) + 1;
        (line, col)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    /// First label is the primary location.
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Diagnostic {
            severity: Severity::Error,
            message: message.into(),
            labels: vec![Label {
                span,
                message: String::new(),
            }],
            notes: vec![],
        }
    }

    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Plain-text rendering: `path:line:col: error: message` plus notes.
    pub fn render(&self, sm: &SourceMap) -> String {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        let mut out = match self.labels.first() {
            Some(l) if (l.span.file.0 as usize) < sm.files.len() => {
                let (line, col) = sm.line_col(l.span.file, l.span.lo);
                format!(
                    "{}:{}:{}: {}: {}",
                    sm.get(l.span.file).path.display(),
                    line,
                    col,
                    sev,
                    self.message
                )
            }
            _ => format!("{}: {}", sev, self.message),
        };
        for l in self.labels.iter().skip(1).filter(|l| !l.message.is_empty()) {
            if (l.span.file.0 as usize) < sm.files.len() {
                let (line, col) = sm.line_col(l.span.file, l.span.lo);
                out.push_str(&format!(
                    "\n  --> {}:{}:{}: {}",
                    sm.get(l.span.file).path.display(),
                    line,
                    col,
                    l.message
                ));
            }
        }
        for n in &self.notes {
            out.push_str(&format!("\n  = note: {}", n));
        }
        out
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

pub type Diagnostics = Vec<Diagnostic>;
