//! An in-memory terminal emulator for one thread's terminal output.
//!
//! It parses output with vte 0.15.0 into alacritty_terminal 0.26.0's `Term`, and draws the
//! visible screen into a ratatui buffer. It opens no PTY, starts no thread, writes nothing to
//! the host terminal and touches no clipboard or window. Replies to the program's queries wait
//! in a bounded queue for the caller. docs/terminal-emulator.md covers the limits, and what
//! `reset` and `clear` keep.

mod guard;
mod handler;
mod render;
#[cfg(test)]
mod tests;

use std::fmt;
use std::time::Instant;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::{Config, Osc52, TermMode};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use guard::OscGuard;
use handler::{GuardedTerm, ReplyListener};

/// The emulator keeps its size within these bounds. alacritty_terminal can't draw a wide
/// character in one column, so two is the narrowest.
pub const MIN_COLUMNS: u16 = 2;
pub const MAX_COLUMNS: u16 = 1000;
pub const MIN_ROWS: u16 = 1;
pub const MAX_ROWS: u16 = 500;

/// Scrollback is capped at this many rows, and at `MAX_SCROLLBACK_CELLS` divided by the width.
const MAX_SCROLLBACK_ROWS: usize = 5000;
const MAX_SCROLLBACK_CELLS: usize = 200_000;

/// The terminal for the open thread. Feed it the thread's output with `reset` and `apply`, and
/// draw it with `render`.
pub struct TerminalEmulator {
    engine: Engine,
    columns: u16,
    rows: u16,
    write_access: bool,
}

/// Where to draw the cursor, in buffer coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalCursor {
    pub x: u16,
    pub y: u16,
    pub shape: TerminalCursorShape,
    pub blinking: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalCursorShape {
    Block,
    Underline,
    Bar,
}

/// The modes the program set that change what keys, pastes and the mouse should send it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputModes {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub alternate_screen: bool,
    /// The mouse wheel sends arrow keys while the alternate screen is up.
    pub alternate_scroll: bool,
    /// A line feed also returns the cursor to the first column.
    pub line_feed_new_line: bool,
    pub mouse: MouseTracking,
    pub mouse_encoding: MouseEncoding,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseTracking {
    #[default]
    Off,
    /// Presses and releases.
    Click,
    /// Presses, releases and motion with a button held.
    Drag,
    /// Presses, releases and all motion.
    Motion,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

/// One parse of the output: the screen, the parser and its guard, and the reply queue. `reset`
/// and `clear` replace it whole, so nothing from an earlier parse, a reply included, outlives
/// them.
struct Engine {
    term: Term<ReplyListener>,
    processor: Processor<StdSyncHandler>,
    guard: OscGuard,
    listener: ReplyListener,
}

impl Engine {
    fn new(columns: u16, rows: u16) -> Self {
        let listener = ReplyListener::default();
        let size = (usize::from(rows), usize::from(columns));
        Self {
            term: Term::new(config(columns), &size, listener.clone()),
            processor: Processor::new(),
            guard: OscGuard::default(),
            listener,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        let Self {
            term,
            processor,
            guard,
            ..
        } = self;
        let mut handler = GuardedTerm { term };
        guard.filter(bytes, |run| processor.advance(&mut handler, run));
    }

    fn sync_deadline(&self) -> Option<Instant> {
        self.processor.sync_timeout().sync_timeout()
    }

    /// Ends a synchronized update, applying what the parser held back.
    fn flush_sync(&mut self) {
        if self.sync_deadline().is_some() {
            self.processor.stop_sync(&mut GuardedTerm {
                term: &mut self.term,
            });
        }
    }
}

fn config(columns: u16) -> Config {
    Config {
        scrolling_history: (MAX_SCROLLBACK_CELLS / usize::from(columns)).min(MAX_SCROLLBACK_ROWS),
        kitty_keyboard: false,
        osc52: Osc52::Disabled,
        ..Config::default()
    }
}

fn clamp(columns: u16, rows: u16) -> (u16, u16) {
    (
        columns.clamp(MIN_COLUMNS, MAX_COLUMNS),
        rows.clamp(MIN_ROWS, MAX_ROWS),
    )
}

impl TerminalEmulator {
    /// An empty terminal, without write access. The size is clamped to the bounds above.
    pub fn new(columns: u16, rows: u16) -> Self {
        let (columns, rows) = clamp(columns, rows);
        Self {
            engine: Engine::new(columns, rows),
            columns,
            rows,
            write_access: false,
        }
    }

    /// Columns and rows, after clamping.
    pub fn size(&self) -> (u16, u16) {
        (self.columns, self.rows)
    }

    /// Resizes the screen and reflows its lines. The scrollback cap follows the new width.
    pub fn resize(&mut self, columns: u16, rows: u16) {
        let (columns, rows) = clamp(columns, rows);
        if (columns, rows) == (self.columns, self.rows) {
            return;
        }
        self.columns = columns;
        self.rows = rows;
        // Setting the cap first trims old rows before a wider reflow copies them.
        self.engine.term.set_options(config(columns));
        self.engine
            .term
            .resize((usize::from(rows), usize::from(columns)));
    }

    /// Starts over from `history`, the thread's whole output so far, at the current size. The
    /// screen, modes, colors and scrollback all come from `history` alone. Replies waiting in
    /// the queue are dropped, and replaying `history` queues none. A synchronized update still
    /// open at its end is applied.
    pub fn reset(&mut self, history: &str) {
        self.engine = Engine::new(self.columns, self.rows);
        self.engine.feed(history.as_bytes());
        self.engine.flush_sync();
        self.engine.listener.queue().accepting = self.write_access;
    }

    /// An empty screen in the default modes, as if `reset` had no history.
    pub fn clear(&mut self) {
        self.reset("");
    }

    /// Feeds new output. A sequence split across two calls parses as if it came whole.
    pub fn apply(&mut self, output: &str) {
        self.engine.feed(output.as_bytes());
    }

    /// Whether replies to the program's queries are kept for the caller to send. Output already
    /// held back by a synchronized update is applied first, under the old setting. Taking write
    /// access away drops the replies not yet taken.
    pub fn set_write_access(&mut self, write_access: bool) {
        if write_access == self.write_access {
            return;
        }
        self.engine.flush_sync();
        self.write_access = write_access;
        let mut queue = self.engine.listener.queue();
        if !write_access {
            queue.text.clear();
            queue.count = 0;
        }
        queue.accepting = write_access;
    }

    pub fn write_access(&self) -> bool {
        self.write_access
    }

    /// Replies to send to the program, in order, joined into one string.
    pub fn take_replies(&mut self) -> Option<String> {
        let mut queue = self.engine.listener.queue();
        queue.count = 0;
        Some(std::mem::take(&mut queue.text)).filter(|text| !text.is_empty())
    }

    /// Replies dropped because the queue was full, since the last `reset`.
    pub fn dropped_replies(&self) -> usize {
        self.engine.listener.queue().dropped
    }

    /// When the synchronized update the program opened should be applied even if the program
    /// hasn't closed it. Until then the parser holds output back, so the caller should call
    /// `flush_sync_if_due` at this time.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.engine.sync_deadline()
    }

    /// Applies held-back output if the synchronized update's deadline is at or before `now`.
    /// Returns whether it did.
    pub fn flush_sync_if_due(&mut self, now: Instant) -> bool {
        match self.engine.sync_deadline() {
            Some(deadline) if deadline <= now => {
                self.engine.flush_sync();
                true
            }
            _ => false,
        }
    }

    /// Scrolls the view by `rows`, up into the scrollback when positive.
    pub fn scroll_viewport(&mut self, rows: i32) {
        // `Grid::scroll_display` adds the delta to the offset as an `i32`. No scrollback is
        // deeper than this bound, so clamping changes nothing but the overflow.
        let bound = MAX_SCROLLBACK_ROWS as i32;
        let rows = rows.clamp(-bound, bound);
        self.engine.term.scroll_display(Scroll::Delta(rows));
    }

    /// Rows the view is scrolled up from the bottom.
    pub fn viewport_offset(&self) -> usize {
        self.engine.term.grid().display_offset()
    }

    pub fn input_modes(&self) -> InputModes {
        let mode = *self.engine.term.mode();
        let mouse = if mode.contains(TermMode::MOUSE_MOTION) {
            MouseTracking::Motion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseTracking::Drag
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseTracking::Click
        } else {
            MouseTracking::Off
        };
        let mouse_encoding = if mode.contains(TermMode::SGR_MOUSE) {
            MouseEncoding::Sgr
        } else if mode.contains(TermMode::UTF8_MOUSE) {
            MouseEncoding::Utf8
        } else {
            MouseEncoding::Default
        };
        InputModes {
            application_cursor: mode.contains(TermMode::APP_CURSOR),
            application_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            focus_events: mode.contains(TermMode::FOCUS_IN_OUT),
            alternate_screen: mode.contains(TermMode::ALT_SCREEN),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            line_feed_new_line: mode.contains(TermMode::LINE_FEED_NEW_LINE),
            mouse,
            mouse_encoding,
        }
    }

    /// Draws the visible rows into `buf`, from the top-left of `area`. Every cell of `area`
    /// inside `buf` is overwritten, and nothing outside either. Returns the cursor if it shows
    /// within both.
    pub fn render(&self, area: Rect, buf: &mut Buffer) -> Option<TerminalCursor> {
        render::render(&self.engine.term, area, buf)
    }
}

impl fmt::Debug for TerminalEmulator {
    /// Sizes and counts only. Output and replies can hold secrets.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let queue = self.engine.listener.queue();
        f.debug_struct("TerminalEmulator")
            .field("columns", &self.columns)
            .field("rows", &self.rows)
            .field("write_access", &self.write_access)
            .field("pending_replies", &queue.count)
            .field("pending_reply_bytes", &queue.text.len())
            .field("dropped_replies", &queue.dropped)
            .field("sync_pending", &self.engine.sync_deadline().is_some())
            .finish()
    }
}
