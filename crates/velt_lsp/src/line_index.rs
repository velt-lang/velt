//! Byte offsets (what spans use) ↔ LSP positions (line + UTF-16 code unit column, what editors use).
//! Every conversion clamps instead of failing: clients may send positions past the end of a line
//! or of the document while typing.

use lsp_types::{Position, Range};

/// Line start offsets of one text, for converting in both directions.
pub struct LineIndex<'a> {
    text: &'a str,
    /// Byte offset of the first character of each line (`[0]` is always 0).
    starts: Vec<u32>,
}

impl<'a> LineIndex<'a> {
    /// Index the lines of `text` (`\n` terminated; a preceding `\r` stays part of the line).
    pub fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i as u32 + 1),
        );
        LineIndex { text, starts }
    }

    /// LSP position of byte `offset` (clamped to the text, rounded down to a char boundary).
    pub fn position(&self, offset: u32) -> Position {
        let offset = floor_boundary(self.text, offset as usize);
        let line = self.starts.partition_point(|&s| s as usize <= offset) - 1;
        let start = self.starts[line] as usize;
        let character: usize = self.text[start..offset].chars().map(char::len_utf16).sum();
        Position::new(line as u32, character as u32)
    }

    /// LSP range of the byte range `lo..hi`.
    pub fn range(&self, lo: u32, hi: u32) -> Range {
        Range::new(self.position(lo), self.position(hi.max(lo)))
    }

    /// Byte offset of `pos`; columns past the end of the line clamp to the line end.
    pub fn offset(&self, pos: Position) -> u32 {
        let Some(&start) = self.starts.get(pos.line as usize) else {
            return self.text.len() as u32;
        };
        let line_end = self
            .starts
            .get(pos.line as usize + 1)
            .map_or(self.text.len(), |&next| next as usize - 1);
        let line = &self.text[start as usize..line_end];
        let mut units = 0;
        for (i, c) in line.char_indices() {
            if units >= pos.character as usize {
                return start + i as u32;
            }
            units += c.len_utf16();
        }
        start + line.trim_end_matches('\r').len() as u32
    }

    /// Range covering the whole text.
    pub fn full_range(&self) -> Range {
        self.range(0, self.text.len() as u32)
    }
}

fn floor_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_columns_both_ways() {
        // `é` is 2 bytes / 1 UTF-16 unit; `😀` is 4 bytes / 2 units.
        let text = "ab\né😀x\r\nlast";
        let idx = LineIndex::new(text);
        let x = text.find('x').unwrap() as u32;
        assert_eq!(idx.position(x), Position::new(1, 3));
        assert_eq!(idx.offset(Position::new(1, 3)), x);
        assert_eq!(idx.position(0), Position::new(0, 0));
        let last = text.find("last").unwrap() as u32;
        assert_eq!(idx.position(last), Position::new(2, 0));
        assert_eq!(idx.offset(Position::new(2, 2)), last + 2);
    }

    #[test]
    fn clamps_out_of_range_positions() {
        let text = "ab\r\ncd";
        let idx = LineIndex::new(text);
        assert_eq!(idx.offset(Position::new(0, 99)), 2, "stops before \\r\\n");
        assert_eq!(idx.offset(Position::new(7, 0)), text.len() as u32);
        assert_eq!(idx.position(999), Position::new(1, 2));
        // Inside a multi-byte char: rounds down.
        let idx = LineIndex::new("é");
        assert_eq!(idx.position(1), Position::new(0, 0));
    }
}
