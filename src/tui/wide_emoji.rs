//! The terminal backend, with each emoji that ends in U+FE0F drawn so the rest of its row stays
//! in place.
//!
//! Terminals disagree on how wide such an emoji is. Ghostty, kitty, WezTerm and tmux 3.6a draw ⚠
//! and U+FE0F two wide, as unicode-width counts it, and tmux draws it a column wide when its
//! `variation-selector-always-wide` option is off. ratatui 0.30 writes the cell that follows it
//! without moving the cursor first (ratatui-core 0.1.2, src/buffer/diff.rs:158-170), which is right
//! where the terminal draws it a column wide. Where the terminal draws it two wide, that cell and
//! the rest of the row land a column to the right, and the last of them can cover the border.
//!
//! So this backend writes the cell after the emoji first, then moves back and writes the emoji,
//! then moves to the next cell it writes. A terminal that draws the emoji a column wide shows
//! the cell after it, and one that draws it two wide covers that cell with it. Either way the
//! next cell is where ratatui put it.

use std::io;

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use unicode_width::UnicodeWidthStr;

/// A backend, but each emoji that ends in U+FE0F is drawn as the module says.
pub(super) struct WideEmoji<B>(pub(super) B);

/// Whether `cell` holds an emoji with U+FE0F that ratatui counts two columns wide, which some
/// terminals draw one wide.
fn disputed(cell: &Cell) -> bool {
    let symbol = cell.symbol();
    symbol.contains('\u{fe0f}') && symbol.width() > 1
}

impl<B: Backend<Error = io::Error>> Backend for WideEmoji<B> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let cells: Vec<(u16, u16, &Cell)> = content.collect();
        // Each run goes to the backend in a call of its own, which starts with a move.
        let mut start = 0;
        let mut i = 0;
        while i < cells.len() {
            let (x, y, cell) = cells[i];
            if !disputed(cell) {
                i += 1;
                continue;
            }
            if start < i {
                self.0.draw(cells[start..i].iter().copied())?;
            }
            // The cells it covers that ratatui writes: the one after it, never one past the row.
            let end = x.saturating_add(cell.symbol().width() as u16);
            let mut after = i + 1;
            while after < cells.len() && cells[after].1 == y && cells[after].0 < end {
                after += 1;
            }
            if i + 1 < after {
                self.0.draw(cells[i + 1..after].iter().copied())?;
            }
            self.0.draw(std::iter::once(cells[i]))?;
            start = after;
            i = after;
        }
        if start < cells.len() {
            self.0.draw(cells[start..].iter().copied())?;
        }
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.0.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.0.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.0.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.0.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.0.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.0.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.0.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.0.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.0.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;
    use unicode_width::UnicodeWidthChar;

    use super::*;

    /// A terminal's screen that reads what a backend wrote, drawing an emoji with U+FE0F one
    /// column wide, as tmux does with `variation-selector-always-wide` off, or two, as Ghostty
    /// does. It knows only moves and text, which is all the backend writes but colors.
    struct Screen {
        wide: bool,
        /// Each cell's text, and `None` for the right half of a wide one.
        rows: Vec<Vec<Option<String>>>,
        at: (usize, usize),
        /// The cell last written, which a selector or joiner after it belongs to.
        last: Option<(usize, usize)>,
    }

    impl Screen {
        fn new(width: usize, height: usize, wide: bool) -> Screen {
            let rows = vec![vec![Some(" ".to_string()); width]; height];
            Screen {
                wide,
                rows,
                at: (0, 0),
                last: None,
            }
        }

        fn read(&mut self, bytes: &[u8]) {
            let text = std::str::from_utf8(bytes).unwrap();
            let mut chars = text.chars().peekable();
            while let Some(c) = chars.next() {
                if c != '\u{1b}' {
                    self.print(c);
                    continue;
                }
                assert_eq!(chars.next(), Some('['), "only CSI sequences");
                let mut params = String::new();
                let end = loop {
                    let c = chars.next().unwrap();
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break c;
                    }
                    params.push(c);
                };
                if end == 'H' {
                    let (row, column) = params.split_once(';').unwrap();
                    let row: usize = row.parse().unwrap();
                    let column: usize = column.parse().unwrap();
                    self.at = (column - 1, row - 1);
                    self.last = None;
                }
            }
        }

        /// Clears whatever wide cell covers column `x` of row `y`, as a terminal does when a
        /// write lands on half of one.
        fn split(&mut self, x: usize, y: usize) {
            let row = &mut self.rows[y];
            if x >= row.len() {
                return;
            }
            if row[x].is_none() && x > 0 {
                row[x - 1] = Some(" ".to_string());
                row[x] = Some(" ".to_string());
            } else if x + 1 < row.len() && row[x + 1].is_none() {
                row[x + 1] = Some(" ".to_string());
            }
        }

        /// Writes `c` at the cursor. Past the last column it writes over the last one, as a
        /// terminal without autowrap does.
        fn print(&mut self, c: char) {
            if c == '\u{fe0f}' {
                let (lx, ly) = self.last.expect("a selector after a character");
                self.rows[ly][lx].as_mut().unwrap().push(c);
                if self.wide && lx + 1 < self.rows[ly].len() && self.rows[ly][lx + 1].is_some() {
                    self.split(lx + 1, ly);
                    self.rows[ly][lx + 1] = None;
                    self.at.0 = lx + 2;
                }
                return;
            }
            let width = c.width().unwrap_or(0);
            if width == 0 {
                let (lx, ly) = self.last.expect("a joiner after a character");
                self.rows[ly][lx].as_mut().unwrap().push(c);
                return;
            }
            let y = self.at.1;
            let x = self.at.0.min(self.rows[y].len() - width);
            self.split(x, y);
            if width == 2 {
                self.split(x + 1, y);
            }
            self.rows[y][x] = Some(c.to_string());
            if width == 2 && x + 1 < self.rows[y].len() {
                self.rows[y][x + 1] = None;
            }
            self.last = Some((x, y));
            self.at.0 = x + width;
        }

        /// Each row as the screen shows it.
        fn text(&self) -> Vec<String> {
            self.rows
                .iter()
                .map(|row| row.iter().flatten().cloned().collect())
                .collect()
        }
    }

    /// Each row of `buffer` as a terminal should show it: a wide cell hides the one after it,
    /// except an emoji with U+FE0F where the terminal draws it a column wide.
    fn expected(buffer: &Buffer, wide: bool) -> Vec<String> {
        let area = buffer.area;
        (area.y..area.bottom())
            .map(|y| {
                let mut row = String::new();
                let mut x = area.x;
                while x < area.right() {
                    let cell = &buffer[(x, y)];
                    row.push_str(cell.symbol());
                    let width = cell.symbol().width().max(1) as u16;
                    x += if disputed(cell) && !wide { 1 } else { width };
                }
                row
            })
            .collect()
    }

    /// Frames that put an emoji with U+FE0F over text, move it a column, give it a color of its
    /// own, put one at the end of a row and two beside each other, then take them away.
    fn frames() -> Vec<Vec<Line<'static>>> {
        let warn = "\u{26a0}\u{fe0f}";
        let heart = "\u{2764}\u{fe0f}";
        vec![
            vec![Line::from("abcdefghij"), Line::from("klmnopqrst")],
            vec![
                Line::from(format!("a{warn}bcdefg")),
                Line::from(format!("k{heart}{warn}lmnop")),
            ],
            vec![
                Line::from(format!("ab{warn}cdefg")),
                Line::from(format!("klmnopqr{heart}")),
            ],
            vec![
                Line::from(vec![
                    Span::raw("ab"),
                    Span::styled(warn, Style::new().fg(Color::Red)),
                    Span::raw(" 2 中x"),
                ]),
                Line::from(format!("{heart} 1  {warn} 2")),
            ],
            vec![Line::from("abcdefghij"), Line::from("klmnopqrst")],
        ]
    }

    /// Draws each frame through `backend` and checks the screen shows it, in a terminal that
    /// draws the emoji `wide` or not.
    fn shows_each_frame<B: Backend<Error = io::Error>>(
        backend: B,
        written: &Written,
        wide: bool,
    ) -> Vec<(Vec<String>, Vec<String>)> {
        let mut terminal = Terminal::new(backend).unwrap();
        let mut screen = Screen::new(10, 2, wide);
        let mut seen = Vec::new();
        for lines in frames() {
            let frame = terminal
                .draw(|frame| frame.render_widget(Paragraph::new(lines.clone()), frame.area()))
                .unwrap();
            let buffer = frame.buffer.clone();
            screen.read(&std::mem::take(&mut *written.0.borrow_mut()));
            seen.push((screen.text(), expected(&buffer, wide)));
        }
        seen
    }

    /// What a backend wrote, which the test reads after each frame.
    #[derive(Clone, Default)]
    struct Written(Rc<RefCell<Vec<u8>>>);

    impl io::Write for Written {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_row_stays_in_place_whether_the_terminal_draws_the_emoji_one_or_two_wide() {
        for wide in [false, true] {
            let written = Written::default();
            let backend = Sized10x2(WideEmoji(CrosstermBackend::new(written.clone())));
            let seen = shows_each_frame(backend, &written, wide);
            for (n, (shown, want)) in seen.iter().enumerate() {
                assert_eq!(shown, want, "frame {n}, wide {wide}");
            }
        }
    }

    #[test]
    fn ratatui_alone_moves_the_row_where_the_terminal_draws_the_emoji_two_wide() {
        // What this backend is for: without it, a terminal that draws the emoji two wide shows
        // the rest of the row a column to the right.
        let written = Written::default();
        let backend = Sized10x2(CrosstermBackend::new(written.clone()));
        let seen = shows_each_frame(backend, &written, true);
        // The cell after ⚠ lands a column right, and the last falls off the row.
        let (shown, want) = &seen[1];
        assert_eq!(want[0], "a\u{26a0}\u{fe0f}bcdefg ");
        assert_eq!(shown[0], "a\u{26a0}\u{fe0f} bcdef ");
        let written = Written::default();
        let backend = Sized10x2(CrosstermBackend::new(written.clone()));
        let seen = shows_each_frame(backend, &written, false);
        assert!(seen.iter().all(|(shown, want)| shown == want));
    }

    #[test]
    fn only_an_emoji_with_the_selector_that_ratatui_counts_two_wide_is_disputed() {
        let cell = |symbol: &str| {
            let mut cell = Cell::default();
            cell.set_symbol(symbol);
            cell
        };
        assert!(disputed(&cell("\u{26a0}\u{fe0f}")));
        assert!(disputed(&cell("\u{2764}\u{fe0f}")));
        // Wide everywhere, narrow everywhere, or joined.
        assert!(!disputed(&cell("👍")));
        assert!(!disputed(&cell("中")));
        assert!(!disputed(&cell("\u{2764}")));
        assert!(!disputed(&cell("a")));
        assert!(!disputed(&cell("👩\u{200d}💻")));
    }

    /// A backend 10 columns by 2 rows, where CrosstermBackend would ask the real terminal.
    struct Sized10x2<B>(B);

    impl<B: Backend<Error = io::Error>> Backend for Sized10x2<B> {
        type Error = io::Error;

        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            self.0.draw(content)
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            Ok(Position::ORIGIN)
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, _: P) -> io::Result<()> {
            Ok(())
        }

        fn clear(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn clear_region(&mut self, _: ClearType) -> io::Result<()> {
            Ok(())
        }

        fn size(&self) -> io::Result<Size> {
            Ok(Size::new(10, 2))
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            Ok(WindowSize {
                columns_rows: Size::new(10, 2),
                pixels: Size::new(0, 0),
            })
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }
}
