//! A multiline text input with a cursor, wrapped to the pane width.

use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub struct Composer {
    chars: Vec<char>,
    cursor: usize,
}

impl Composer {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.iter().all(|c| c.is_whitespace())
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, text: &str) {
        for c in text.chars().filter(|c| *c != '\r') {
            self.insert(c);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    fn line_start(&self, at: usize) -> usize {
        self.chars[..at]
            .iter()
            .rposition(|c| *c == '\n')
            .map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.chars[at..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(self.chars.len(), |i| at + i)
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    /// Moves to the same column on the previous logical line. Returns false on the first line.
    pub fn up(&mut self) -> bool {
        let start = self.line_start(self.cursor);
        if start == 0 {
            return false;
        }
        let column = self.cursor - start;
        let previous_start = self.line_start(start - 1);
        self.cursor = (previous_start + column).min(start - 1);
        true
    }

    pub fn down(&mut self) -> bool {
        let end = self.line_end(self.cursor);
        if end == self.chars.len() {
            return false;
        }
        let column = self.cursor - self.line_start(self.cursor);
        self.cursor = (end + 1 + column).min(self.line_end(end + 1));
        true
    }

    pub fn delete_word(&mut self) {
        while self.cursor > 0 && self.chars[self.cursor - 1].is_whitespace() {
            self.backspace();
        }
        while self.cursor > 0 && !self.chars[self.cursor - 1].is_whitespace() {
            self.backspace();
        }
    }

    /// Rows wrapped by display width, and the cursor's (row, column).
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(2);
        let mut rows = vec![String::new()];
        let mut row_width = 0;
        let mut cursor = (0, 0);
        for (index, c) in self.chars.iter().enumerate() {
            if index == self.cursor {
                cursor = (rows.len() - 1, row_width);
            }
            if *c == '\n' {
                rows.push(String::new());
                row_width = 0;
                continue;
            }
            let w = c.width().unwrap_or(0);
            if row_width + w > width {
                rows.push(String::new());
                row_width = 0;
            }
            rows.last_mut().expect("rows is never empty").push(*c);
            row_width += w;
        }
        if self.cursor == self.chars.len() {
            if row_width >= width {
                rows.push(String::new());
                row_width = 0;
            }
            cursor = (rows.len() - 1, row_width);
        }
        (rows, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_between_lines_and_reports_wrapped_cursor() {
        let mut composer = Composer::default();
        composer.insert_str("hello\nhi");
        assert!(composer.up());
        assert_eq!(composer.layout(80).1, (0, 2));
        assert!(composer.down());
        composer.end();
        assert_eq!(composer.layout(80).1, (1, 2));

        composer.clear();
        composer.insert_str("abcdef");
        let (rows, cursor) = composer.layout(4);
        assert_eq!(rows, vec!["abcd", "ef"]);
        assert_eq!(cursor, (1, 2));
    }
}
