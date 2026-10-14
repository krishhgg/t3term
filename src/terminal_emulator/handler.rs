//! What the parser drives: Alacritty's `Term`, behind a wrapper that keeps the effects a
//! viewer shouldn't have, and a listener that keeps only bounded replies.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::cursor_icon::CursorIcon;
use alacritty_terminal::vte::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys, PrivateMode, Rgb,
    ScpCharPath, ScpUpdateMode, StandardCharset, TabulationClearMode,
};
use unicode_width::UnicodeWidthChar;

/// Zero-width characters one cell may hold. More are dropped.
pub(super) const MAX_ZERO_WIDTH_PER_CELL: usize = 16;
/// Replies waiting for the caller, by count and by bytes. Replies past either are dropped.
pub(super) const MAX_PENDING_REPLIES: usize = 32;
pub(super) const MAX_PENDING_REPLY_BYTES: usize = 4096;

/// Replies `Term` asked to write back to the program, waiting for the caller.
#[derive(Debug, Default)]
pub(super) struct ReplyQueue {
    /// Whether replies are kept at all. Only live output, with write access, keeps them.
    pub(super) accepting: bool,
    pub(super) text: String,
    pub(super) count: usize,
    /// Replies dropped because the queue was full.
    pub(super) dropped: usize,
}

/// Receives `Term`'s events. It keeps the text of `PtyWrite` while the queue accepts it and
/// has room, and drops every other event, so nothing reaches the clipboard, the window title
/// or the host terminal. It never calls the closures some events carry.
#[derive(Clone, Debug, Default)]
pub(super) struct ReplyListener {
    queue: Arc<Mutex<ReplyQueue>>,
}

impl ReplyListener {
    pub(super) fn queue(&self) -> MutexGuard<'_, ReplyQueue> {
        // Nothing panics while holding the lock, but a poisoned queue is still sound to use.
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl EventListener for ReplyListener {
    fn send_event(&self, event: Event) {
        let Event::PtyWrite(reply) = event else {
            return;
        };
        let mut queue = self.queue();
        if !queue.accepting {
            return;
        }
        if queue.count >= MAX_PENDING_REPLIES
            || queue.text.len() + reply.len() > MAX_PENDING_REPLY_BYTES
        {
            queue.dropped = queue.dropped.saturating_add(1);
            return;
        }
        queue.text.push_str(&reply);
        queue.count += 1;
    }
}

/// Passes each parser action to `Term`, except:
///
/// - a zero-width character whose target cell already holds `MAX_ZERO_WIDTH_PER_CELL` of them,
/// - window titles and the title stack,
/// - hyperlinks,
/// - clipboard reads and writes,
/// - the text-area size in pixels, which only a window could answer.
pub(super) struct GuardedTerm<'a> {
    pub(super) term: &'a mut Term<ReplyListener>,
}

impl GuardedTerm<'_> {
    /// Whether the cell `Term::input` in alacritty_terminal 0.26.0 would add a zero-width
    /// character to is full. It picks the cell before the cursor, or the cursor's own cell when
    /// the next character wraps, and moves left off the second half of a wide character.
    fn zero_width_target_full(&self) -> bool {
        let grid = self.term.grid();
        let cursor = &grid.cursor;
        let row = &grid[cursor.point.line];
        let mut column = cursor.point.column;
        if !cursor.input_needs_wrap {
            column.0 = column.0.saturating_sub(1);
        }
        if row[column].flags.contains(Flags::WIDE_CHAR_SPACER) {
            column.0 = column.0.saturating_sub(1);
        }
        row[column].zerowidth().map_or(0, <[char]>::len) >= MAX_ZERO_WIDTH_PER_CELL
    }
}

impl Handler for GuardedTerm<'_> {
    fn set_title(&mut self, _: Option<String>) {}

    fn set_cursor_style(&mut self, style: Option<CursorStyle>) {
        self.term.set_cursor_style(style);
    }

    fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.term.set_cursor_shape(shape);
    }

    fn input(&mut self, c: char) {
        if c.width() == Some(0) && self.zero_width_target_full() {
            return;
        }
        self.term.input(c);
    }

    fn goto(&mut self, line: i32, col: usize) {
        self.term.goto(line, col);
    }

    fn goto_line(&mut self, line: i32) {
        self.term.goto_line(line);
    }

    fn goto_col(&mut self, col: usize) {
        self.term.goto_col(col);
    }

    fn insert_blank(&mut self, count: usize) {
        self.term.insert_blank(count);
    }

    fn move_up(&mut self, rows: usize) {
        self.term.move_up(rows);
    }

    fn move_down(&mut self, rows: usize) {
        self.term.move_down(rows);
    }

    fn identify_terminal(&mut self, intermediate: Option<char>) {
        self.term.identify_terminal(intermediate);
    }

    fn device_status(&mut self, arg: usize) {
        self.term.device_status(arg);
    }

    fn move_forward(&mut self, cols: usize) {
        self.term.move_forward(cols);
    }

    fn move_backward(&mut self, cols: usize) {
        self.term.move_backward(cols);
    }

    fn move_down_and_cr(&mut self, rows: usize) {
        self.term.move_down_and_cr(rows);
    }

    fn move_up_and_cr(&mut self, rows: usize) {
        self.term.move_up_and_cr(rows);
    }

    fn put_tab(&mut self, count: u16) {
        self.term.put_tab(count);
    }

    fn backspace(&mut self) {
        self.term.backspace();
    }

    fn carriage_return(&mut self) {
        self.term.carriage_return();
    }

    fn linefeed(&mut self) {
        self.term.linefeed();
    }

    fn bell(&mut self) {
        self.term.bell();
    }

    fn substitute(&mut self) {
        self.term.substitute();
    }

    fn newline(&mut self) {
        self.term.newline();
    }

    fn set_horizontal_tabstop(&mut self) {
        self.term.set_horizontal_tabstop();
    }

    fn scroll_up(&mut self, rows: usize) {
        self.term.scroll_up(rows);
    }

    fn scroll_down(&mut self, rows: usize) {
        self.term.scroll_down(rows);
    }

    fn insert_blank_lines(&mut self, count: usize) {
        self.term.insert_blank_lines(count);
    }

    fn delete_lines(&mut self, count: usize) {
        self.term.delete_lines(count);
    }

    fn erase_chars(&mut self, count: usize) {
        self.term.erase_chars(count);
    }

    fn delete_chars(&mut self, count: usize) {
        self.term.delete_chars(count);
    }

    fn move_backward_tabs(&mut self, count: u16) {
        self.term.move_backward_tabs(count);
    }

    fn move_forward_tabs(&mut self, count: u16) {
        self.term.move_forward_tabs(count);
    }

    fn save_cursor_position(&mut self) {
        self.term.save_cursor_position();
    }

    fn restore_cursor_position(&mut self) {
        self.term.restore_cursor_position();
    }

    fn clear_line(&mut self, mode: LineClearMode) {
        self.term.clear_line(mode);
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        self.term.clear_screen(mode);
    }

    fn clear_tabs(&mut self, mode: TabulationClearMode) {
        self.term.clear_tabs(mode);
    }

    fn set_tabs(&mut self, interval: u16) {
        self.term.set_tabs(interval);
    }

    fn reset_state(&mut self) {
        self.term.reset_state();
    }

    fn reverse_index(&mut self) {
        self.term.reverse_index();
    }

    fn terminal_attribute(&mut self, attr: Attr) {
        self.term.terminal_attribute(attr);
    }

    fn set_mode(&mut self, mode: Mode) {
        self.term.set_mode(mode);
    }

    fn unset_mode(&mut self, mode: Mode) {
        self.term.unset_mode(mode);
    }

    fn report_mode(&mut self, mode: Mode) {
        self.term.report_mode(mode);
    }

    fn set_private_mode(&mut self, mode: PrivateMode) {
        self.term.set_private_mode(mode);
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        self.term.unset_private_mode(mode);
    }

    fn report_private_mode(&mut self, mode: PrivateMode) {
        self.term.report_private_mode(mode);
    }

    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        self.term.set_scrolling_region(top, bottom);
    }

    fn set_keypad_application_mode(&mut self) {
        self.term.set_keypad_application_mode();
    }

    fn unset_keypad_application_mode(&mut self) {
        self.term.unset_keypad_application_mode();
    }

    fn set_active_charset(&mut self, index: CharsetIndex) {
        self.term.set_active_charset(index);
    }

    fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset) {
        self.term.configure_charset(index, charset);
    }

    fn set_color(&mut self, index: usize, color: Rgb) {
        self.term.set_color(index, color);
    }

    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        self.term.dynamic_color_sequence(prefix, index, terminator);
    }

    fn reset_color(&mut self, index: usize) {
        self.term.reset_color(index);
    }

    fn clipboard_store(&mut self, _: u8, _: &[u8]) {}

    fn clipboard_load(&mut self, _: u8, _: &str) {}

    fn decaln(&mut self) {
        self.term.decaln();
    }

    fn push_title(&mut self) {}

    fn pop_title(&mut self) {}

    fn text_area_size_pixels(&mut self) {}

    fn text_area_size_chars(&mut self) {
        self.term.text_area_size_chars();
    }

    fn set_hyperlink(&mut self, _: Option<Hyperlink>) {}

    fn set_mouse_cursor_icon(&mut self, icon: CursorIcon) {
        self.term.set_mouse_cursor_icon(icon);
    }

    fn report_keyboard_mode(&mut self) {
        self.term.report_keyboard_mode();
    }

    fn push_keyboard_mode(&mut self, mode: KeyboardModes) {
        self.term.push_keyboard_mode(mode);
    }

    fn pop_keyboard_modes(&mut self, to_pop: u16) {
        self.term.pop_keyboard_modes(to_pop);
    }

    fn set_keyboard_mode(&mut self, mode: KeyboardModes, behavior: KeyboardModesApplyBehavior) {
        // `Term` has a private inherent method of the same name.
        Handler::set_keyboard_mode(&mut *self.term, mode, behavior);
    }

    fn set_modify_other_keys(&mut self, mode: ModifyOtherKeys) {
        self.term.set_modify_other_keys(mode);
    }

    fn report_modify_other_keys(&mut self) {
        self.term.report_modify_other_keys();
    }

    fn set_scp(&mut self, char_path: ScpCharPath, update_mode: ScpUpdateMode) {
        self.term.set_scp(char_path, update_mode);
    }
}
