//! The terminal drawer: the open thread's first terminal, `term-1`, under the composer.
//! docs/terminal-pane.md describes what it shows and sends, and its limits.
//!
//! The drawer owns all the work it does for the terminal: one attach stream, one emulator, the
//! input waiting to go out, and the one write and the one resize on their way. Hiding the
//! drawer, opening another thread or quitting drops all of it. Dropping the stream interrupts
//! it on T3 and nothing else, so the shell goes on running. The drawer never opens, restarts,
//! clears or closes a terminal. Start terminal is the one way it asks T3 for a shell, and only
//! while T3 has no `term-1` for the thread.
//!
//! Its timers are one-shot deadlines that only work it has waiting arms: a synchronized update
//! the emulator holds back, a resize waiting for the size to hold still, and an attach after
//! the connection dropped. An idle drawer arms none.

mod keys;
#[cfg(test)]
pub(super) mod tests;

use std::rc::Rc;
use std::time::Duration;

use crossterm::event::KeyEvent;
use futures_util::future::{FutureExt, LocalBoxFuture};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use tokio::time::Instant;
use unicode_width::UnicodeWidthStr;

use super::label;
use super::theme::Theme;
use crate::error::T3Error;
use crate::rpc::{RpcClient, RpcError, StreamLimits};
use crate::terminal::{
    EventBase, Launch, SessionSnapshot, SessionStatus, Size, Terminal, TerminalBatch,
    TerminalError, TerminalEvent, TerminalId, TerminalStream,
};
use crate::terminal_emulator::{self, MouseTracking, TerminalEmulator};

/// The terminal the drawer shows, the first one the desktop opens for a thread.
pub(super) const TERMINAL_ID: &str = "term-1";
/// The title row's button that shows and hides the drawer.
pub(super) const BUTTON: &str = " >_ ";
/// The most rows the drawer takes, its header row included.
const MOST_ROWS: u16 = 17;
/// The rows the transcript keeps under an open drawer.
const TRANSCRIPT_ROWS: u16 = 3;

/// The attach stream's limits. T3 sends up to 8 chunks or 64 KiB before it waits for an Ack,
/// and the first chunk holds the terminal's whole history, at most 8 MiB of text. JSON writes a
/// control character as a six-byte `\u` escape, so that chunk can reach 48 MiB. 16 chunks and
/// 56 MiB hold it and T3's window, under the 64 MiB a WebSocket message may be.
const LIMITS: StreamLimits = StreamLimits::new(16, 56 << 20).unwrap();
/// Input waiting to go out, keys, pastes and the emulator's replies together, is at most this
/// many pieces...
const QUEUED: usize = 256;
/// ...and this many bytes.
const QUEUED_BYTES: usize = 1 << 20;
/// The largest paste the drawer sends. A larger one is refused whole.
const PASTE_BYTES: usize = 256 << 10;
/// How long a new size holds still before the shell gets it.
const RESIZE_DELAY: Duration = Duration::from_millis(100);
/// The wait before the first attach after the connection drops. Each next wait doubles, up to
/// `LAST_RETRY`.
const FIRST_RETRY: Duration = Duration::from_millis(250);
const LAST_RETRY: Duration = Duration::from_secs(10);
/// Attaches after the connection drops before the drawer stops. With the waits above they
/// take about 36 seconds.
const RETRIES: u32 = 8;
/// Rows a turn of the wheel scrolls.
const WHEEL_ROWS: usize = 3;
/// The most columns of a message from T3 or the connection the drawer keeps.
const NOTE: usize = 300;
/// The most columns of the terminal's title the drawer keeps.
const LABEL: usize = 64;

/// Opens the terminal's connection: `Client::terminal_rpc`, or a test's stand-in.
pub(super) type Connect = Rc<dyn Fn() -> LocalBoxFuture<'static, anyhow::Result<RpcClient>>>;

/// The rows the drawer takes when the header, the composer and the status line leave `free`:
/// half of them, at most `MOST_ROWS`, and none of the rows the transcript keeps.
pub(super) fn height(free: u16) -> u16 {
    (free / 2)
        .min(MOST_ROWS)
        .min(free.saturating_sub(TRANSCRIPT_ROWS))
}

/// The drawer for one thread's `term-1`.
pub(super) struct Pane {
    id: TerminalId,
    connect: Connect,
    link: Link,
    /// Attaches tried since the connection dropped, until a snapshot comes.
    retries: u32,
    attaching: Option<LocalBoxFuture<'static, Result<Attached, Failure>>>,
    terminal: Option<Rc<Terminal>>,
    stream: Option<TerminalStream>,
    /// Whether the open stream has sent its snapshot. Until it does, its other events are
    /// skipped and nothing goes to the shell.
    synced: bool,
    /// Whether a snapshot has come at all, so the emulator has a screen to show.
    seen: bool,
    /// The sequence of the last event applied since the last snapshot, when T3 sent one.
    sequence: Option<u64>,
    shell: Shell,
    emulator: TerminalEmulator,
    input: Input,
    writing: Option<LocalBoxFuture<'static, Result<(), TerminalError>>>,
    /// Set when a write failed. Nothing more goes to the shell until a snapshot comes.
    input_failed: bool,
    /// The screen size the last frame drew, and the size last sent to the shell.
    drawn: Option<(u16, u16)>,
    sent: Option<(u16, u16)>,
    /// When the shell should get the drawn size.
    resize_at: Option<Instant>,
    resizing: Option<LocalBoxFuture<'static, Result<(), RpcError>>>,
    /// The latest thing to tell the reader, until the TUI takes it.
    notice: Option<String>,
    /// Where the last frame drew the header row, the screen and Start terminal, for clicks.
    pub(super) header: Rect,
    pub(super) content: Rect,
    pub(super) start: Rect,
}

/// An attach that opened: the terminal on its connection, and its stream.
type Attached = (Rc<Terminal>, TerminalStream);

/// Where the drawer stands with T3.
enum Link {
    /// Connecting and attaching.
    Attaching,
    /// The attach stream is open.
    Open,
    /// T3 has no `term-1` for the thread. The note says why Start terminal didn't start one.
    Missing(Option<String>),
    /// The connection dropped. The drawer attaches again at this time.
    Waiting(Instant),
    /// The drawer stopped for this reason. Enter attaches again.
    Stopped(String),
}

/// Why an attach or its stream ended.
enum Failure {
    /// The connection dropped or the server couldn't be reached, which another attach may get
    /// past.
    Dropped(String),
    /// T3 has no such terminal.
    Missing,
    /// Anything else, which another attach would meet again.
    Stopped(String),
}

/// The shell as the stream last described it.
struct Shell {
    status: Status,
    /// T3's title for the terminal, cleaned and cut.
    label: String,
}

enum Status {
    Starting,
    Running,
    Exited {
        code: Option<i64>,
        signal: Option<i64>,
    },
    /// T3 couldn't start or run the shell, with its message when it sent one.
    Failed(Option<String>),
    Closed,
}

/// What `Pane::next` found, for `Pane::on_update`.
pub(super) struct Update(Happened);

enum Happened {
    Attached(Result<Attached, Failure>),
    Batch(Option<Result<TerminalBatch, RpcError>>),
    Wrote(Result<(), TerminalError>),
    Resized(Result<(), RpcError>),
    Due,
}

/// Input on its way to the shell, in the order it came: keys, pastes and the emulator's
/// replies, joined.
#[derive(Default)]
struct Input {
    text: String,
    pieces: usize,
}

impl Input {
    /// Queues `text` whole, or refuses it whole when the queue has no room for it.
    fn push(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return true;
        }
        if self.pieces >= QUEUED || self.text.len() + text.len() > QUEUED_BYTES {
            return false;
        }
        self.text.push_str(text);
        self.pieces += 1;
        true
    }

    /// Everything queued, as one write.
    fn take(&mut self) -> Option<String> {
        self.pieces = 0;
        Some(std::mem::take(&mut self.text)).filter(|text| !text.is_empty())
    }

    fn clear(&mut self) {
        self.text = String::new();
        self.pieces = 0;
    }
}

impl Pane {
    /// The drawer for terminal `id`, attaching to it at once. The attach names the terminal
    /// and nothing else, so it never starts a shell. The emulator takes its size from the
    /// first frame that draws the drawer.
    pub(super) fn open(id: TerminalId, connect: Connect) -> Pane {
        let mut pane = Pane {
            id,
            connect,
            link: Link::Attaching,
            retries: 0,
            attaching: None,
            terminal: None,
            stream: None,
            synced: false,
            seen: false,
            sequence: None,
            shell: Shell {
                status: Status::Starting,
                label: String::new(),
            },
            emulator: TerminalEmulator::new(80, 24),
            input: Input::default(),
            writing: None,
            input_failed: false,
            drawn: None,
            sent: None,
            resize_at: None,
            resizing: None,
            notice: None,
            header: Rect::default(),
            content: Rect::default(),
            start: Rect::default(),
        };
        pane.attach(None);
        pane
    }

    /// Whether T3 has no `term-1` for the thread, so Enter starts one.
    pub(super) fn missing(&self) -> bool {
        matches!(self.link, Link::Missing(_))
    }

    /// Whether the drawer stopped, so Enter attaches again.
    pub(super) fn stopped(&self) -> bool {
        matches!(self.link, Link::Stopped(_))
    }

    /// Start terminal: attaches with `launch`, so T3 starts a shell from it. Only while T3 has
    /// no `term-1`, and T3 ignores a launch for a terminal that exists, so this never replaces
    /// a shell. A reconnect after it attaches without the launch.
    pub(super) fn start(&mut self, launch: Launch) {
        if self.missing() {
            self.attach(Some(launch));
        }
    }

    /// Notes why Start terminal started nothing.
    pub(super) fn cannot_start(&mut self, reason: &str) {
        if self.missing() {
            self.link = Link::Missing(Some(note(reason)));
        }
    }

    /// Attaches again after the drawer stopped, as Enter does.
    pub(super) fn reconnect(&mut self) {
        if self.stopped() {
            self.retries = 0;
            self.attach(None);
        }
    }

    /// What there is to tell the reader since the last call: the latest notice alone.
    pub(super) fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Forgets where the last frame drew the drawer, so no click lands on it until the next.
    pub(super) fn forget_drawn(&mut self) {
        self.header = Rect::default();
        self.content = Rect::default();
        self.start = Rect::default();
    }

    /// What happens next: the attach opening or failing, a write or a resize ending, a
    /// deadline coming, or a batch from the stream. Cancel-safe: the work lives in the pane,
    /// and dropping this future loses none of it.
    ///
    /// A batch comes last, so a stream that always has one ready holds nothing else back. The
    /// write and the resize on their way are polled first, which sends their requests, and a
    /// deadline that has passed is due at once. Each of the others ends: an attach, a write
    /// and a resize end once, and handling a deadline disarms it. So between two batches the
    /// drawer does only the few things that were waiting, and a batch waits on no timer.
    pub(super) async fn next(&mut self) -> Update {
        let deadline = self.deadline();
        let Pane {
            attaching,
            stream,
            writing,
            resizing,
            ..
        } = self;
        Update(tokio::select! {
            biased;
            attached = finish(attaching) => Happened::Attached(attached),
            wrote = finish(writing) => Happened::Wrote(wrote),
            resized = finish(resizing) => Happened::Resized(resized),
            () = sleep(deadline) => Happened::Due,
            batch = next_batch(stream) => Happened::Batch(batch),
        })
    }

    /// Applies what `next` found and starts the work it allows. Returns whether the drawer
    /// needs drawing again.
    pub(super) fn on_update(&mut self, update: Update) -> bool {
        let redraw = match update.0 {
            Happened::Attached(Ok((terminal, stream))) => {
                self.terminal = Some(terminal);
                self.stream = Some(stream);
                self.link = Link::Open;
                true
            }
            Happened::Attached(Err(failure)) => {
                self.fail(failure);
                true
            }
            Happened::Batch(Some(Ok(batch))) => {
                self.apply(batch);
                true
            }
            Happened::Batch(Some(Err(error))) => {
                self.fail(failure(&error));
                true
            }
            Happened::Batch(None) => {
                self.fail(Failure::Stopped("T3 ended the terminal's stream.".into()));
                true
            }
            Happened::Wrote(Ok(())) => false,
            // The stream sees the connection drop too, and attaches again.
            Happened::Wrote(Err(TerminalError::Rpc(RpcError::Disconnected(_)))) => false,
            Happened::Wrote(Err(error)) => {
                self.input_failed = true;
                self.refresh_access();
                self.notice = Some(note(&format!(
                    "The terminal's input failed, and t3term sends no more until it attaches again: {error}"
                )));
                true
            }
            Happened::Resized(result) => {
                self.resized(result);
                false
            }
            Happened::Due => self.due(),
        };
        self.pump();
        redraw
    }

    /// Types `key` into the shell, or says why it didn't.
    pub(super) fn key(&mut self, key: &KeyEvent) {
        if let Some(text) = keys::encode(key, self.emulator.input_modes()) {
            self.send(&text, "key");
        }
    }

    /// Pastes `text` into the shell whole, or says why nothing went.
    pub(super) fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let text = keys::paste(text, self.emulator.input_modes().bracketed_paste);
        if text.len() > PASTE_BYTES {
            self.notice = Some(format!(
                "t3term pasted nothing: the paste is {} KiB, and the terminal takes at most {} KiB at once.",
                text.len().div_ceil(1024),
                PASTE_BYTES >> 10
            ));
            return;
        }
        self.send(&text, "paste");
    }

    /// Scrolls the view `rows` up into the history, or down toward the prompt when negative.
    pub(super) fn scroll(&mut self, rows: i32) {
        self.emulator.scroll_viewport(rows);
    }

    /// The rows Shift+PgUp and Shift+PgDn scroll: the screen less one.
    pub(super) fn page(&self) -> i32 {
        i32::from(self.emulator.size().1.saturating_sub(1).max(1))
    }

    /// A turn of the wheel over the screen, which scrolls the history. The alternate screen
    /// has none, so there, as in Alacritty, a program that left alternate scroll on and asked
    /// for no mouse reports gets arrow keys instead.
    pub(super) fn wheel(&mut self, up: bool) {
        let modes = self.emulator.input_modes();
        if modes.alternate_screen {
            let arrows = modes.alternate_scroll && modes.mouse == MouseTracking::Off;
            if arrows && self.writable() {
                let arrows = keys::arrows(up, WHEEL_ROWS, modes.application_cursor);
                if self.input.push(&arrows) {
                    self.pump();
                }
            }
            return;
        }
        let rows = WHEEL_ROWS as i32;
        self.scroll(if up { rows } else { -rows });
    }

    /// Draws the drawer in `area`: its header row, then the screen. Returns where the cursor
    /// goes while the drawer has the keyboard. Only the cells in `area` are read or written.
    pub(super) fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        focused: bool,
        theme: &Theme,
    ) -> Option<Position> {
        let area = area.intersection(frame.area());
        self.header = Rect {
            height: area.height.min(1),
            ..area
        };
        self.content = Rect {
            y: area.y + self.header.height,
            height: area.height - self.header.height,
            ..area
        };
        self.start = Rect::default();
        let content = self.content;
        let fits = content.width >= terminal_emulator::MIN_COLUMNS
            && content.height >= terminal_emulator::MIN_ROWS;
        if fits {
            self.fit(content.width, content.height);
        }
        self.draw_header(frame, focused, theme);
        if content.is_empty() {
            return None;
        }
        let canvas = Style::new().bg(theme.bg).fg(theme.fg);
        frame.render_widget(Block::new().style(canvas), content);
        let missing = match &self.link {
            Link::Missing(note) => Some(note.clone()),
            _ => None,
        };
        let lines = if let Some(note) = missing {
            Some(self.missing_lines(note.as_deref(), theme))
        } else if let Link::Stopped(reason) = &self.link {
            Some(vec![
                Line::styled(format!(" {reason}"), Style::new().fg(theme.error_fg)),
                Line::styled(" Enter attaches again.", Style::new().fg(theme.muted)),
            ])
        } else if !self.seen {
            Some(vec![Line::styled(
                " Connecting to the thread's terminal…",
                Style::new().fg(theme.muted),
            )])
        } else {
            None
        };
        if let Some(lines) = lines {
            let text = Paragraph::new(lines)
                .style(canvas)
                .wrap(Wrap { trim: false });
            frame.render_widget(text, content);
            return None;
        }
        if !fits {
            return None;
        }
        let buffer = frame.buffer_mut();
        let cursor = self.emulator.render(content, buffer);
        // The program's default colors are the TUI's.
        for y in content.top()..content.bottom() {
            for x in content.left()..content.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    if cell.fg == Color::Reset {
                        cell.fg = theme.fg;
                    }
                    if cell.bg == Color::Reset {
                        cell.bg = theme.bg;
                    }
                }
            }
        }
        let live = focused && self.writable() && self.emulator.viewport_offset() == 0;
        cursor
            .filter(|_| live)
            .map(|cursor| Position::new(cursor.x, cursor.y))
    }

    /// The header row: the drawer's name, the terminal's title and state, and what the
    /// screen can't show, then F4's key when the row has room.
    fn draw_header(&self, frame: &mut Frame, focused: bool, theme: &Theme) {
        let area = self.header;
        if area.is_empty() {
            return;
        }
        let name = if focused {
            Style::new().fg(theme.primary).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD)
        };
        let muted = Style::new().fg(theme.muted);
        let (state, color) = self.state(theme);
        let mut pieces = vec![(" Terminal".to_string(), name)];
        if !self.shell.label.is_empty() && self.seen {
            pieces.push((format!(" · {}", self.shell.label), muted));
        }
        pieces.push((format!(" · {state}"), Style::new().fg(color)));
        let content = self.content;
        let cue = if content.height == 0 {
            Some("no room for the screen".to_string())
        } else if content.width < terminal_emulator::MIN_COLUMNS {
            Some("too narrow".to_string())
        } else {
            match self.emulator.viewport_offset() {
                0 => None,
                rows => Some(format!("↑ {rows} rows")),
            }
        };
        let width = usize::from(area.width);
        let right = match cue {
            Some(cue) => format!("{cue} · F4 hide "),
            None => "F4 hide ".to_string(),
        };
        let used: usize = pieces.iter().map(|(text, _)| text.width()).sum();
        let right = if used + 1 + right.width() <= width {
            right
        } else {
            String::new()
        };
        let mut room = width.saturating_sub(right.width());
        let mut spans = Vec::new();
        for (text, style) in pieces {
            let text = label::cut(&text, room);
            room -= text.width().min(room);
            spans.push(Span::styled(text, style));
        }
        spans.push(Span::raw(" ".repeat(room)));
        spans.push(Span::styled(right, muted));
        let raised = Style::new().bg(theme.raised);
        frame.render_widget(Paragraph::new(Line::from(spans)).style(raised), area);
    }

    /// What the header says about the terminal, and its color.
    fn state(&self, theme: &Theme) -> (String, Color) {
        match &self.link {
            Link::Attaching | Link::Open if !self.synced => {
                ("attaching".to_string(), theme.warning_fg)
            }
            Link::Attaching | Link::Open => match &self.shell.status {
                Status::Starting => ("starting".to_string(), theme.warning_fg),
                Status::Running if self.input_failed => {
                    ("input stopped".to_string(), theme.error_fg)
                }
                Status::Running => ("running".to_string(), theme.success),
                Status::Exited { code, signal } => {
                    let how = match (code, signal) {
                        (_, Some(signal)) => format!("exited on signal {signal}"),
                        (Some(code), None) => format!("exited with {code}"),
                        (None, None) => "exited".to_string(),
                    };
                    (format!("{how}, read-only"), theme.muted)
                }
                Status::Failed(Some(message)) => {
                    (format!("failed, read-only: {message}"), theme.error_fg)
                }
                Status::Failed(None) => ("failed, read-only".to_string(), theme.error_fg),
                Status::Closed => ("closed, read-only".to_string(), theme.muted),
            },
            Link::Missing(_) => ("not started".to_string(), theme.muted),
            Link::Waiting(_) => (
                format!("reconnecting, try {} of {RETRIES}", self.retries),
                theme.warning_fg,
            ),
            Link::Stopped(_) => ("stopped".to_string(), theme.error_fg),
        }
    }

    /// What a thread without a terminal shows: a line saying so, Start terminal, and why the
    /// last start didn't happen. Notes where Start terminal is for clicks.
    fn missing_lines(&mut self, note: Option<&str>, theme: &Theme) -> Vec<Line<'static>> {
        const START: &str = " Start terminal ";
        let content = self.content;
        let mut lines = Vec::new();
        if content.height > 1 {
            lines.push(Line::styled(
                " This thread has no terminal yet.",
                Style::new().fg(theme.muted),
            ));
        }
        let button = Rect::new(
            content.x + 1,
            content.y + lines.len() as u16,
            START.width() as u16,
            1,
        );
        self.start = button.intersection(content);
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                START,
                Style::new()
                    .fg(theme.primary)
                    .bg(theme.chip_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Enter", Style::new().fg(theme.muted)),
        ]));
        if let Some(note) = note {
            lines.push(Line::styled(
                format!(" {note}"),
                Style::new().fg(theme.error_fg),
            ));
        }
        lines
    }

    /// Gives the emulator the screen's size, `columns` by `rows`, and the shell too once the
    /// size holds still for `RESIZE_DELAY`.
    fn fit(&mut self, columns: u16, rows: u16) {
        let size = (
            columns.min(terminal_emulator::MAX_COLUMNS),
            rows.min(terminal_emulator::MAX_ROWS),
        );
        self.emulator.resize(size.0, size.1);
        if self.drawn != Some(size) {
            self.drawn = Some(size);
            self.resize_at = Some(Instant::now() + RESIZE_DELAY);
        }
    }

    /// Drops the stream and every piece of work on it, and attaches with `launch`, if any.
    fn attach(&mut self, launch: Option<Launch>) {
        self.end_stream();
        self.link = Link::Attaching;
        let (connect, id) = (self.connect.clone(), self.id.clone());
        let attaching = async move {
            let rpc = connect().await.map_err(|error| connect_failure(&error))?;
            let terminal = Rc::new(Terminal::new(rpc, id));
            let attach = terminal.attach(launch.as_ref(), None, LIMITS);
            let stream = attach.await.map_err(|error| failure(&error))?;
            Ok((terminal, stream))
        };
        self.attaching = Some(attaching.boxed_local());
    }

    /// Drops the stream, which interrupts it on T3, and the attach, write and resize on their
    /// way, and the input waiting. The screen stays until a snapshot replaces it.
    fn end_stream(&mut self) {
        self.attaching = None;
        self.stream = None;
        self.terminal = None;
        self.writing = None;
        self.resizing = None;
        self.synced = false;
        self.sent = None;
        self.refresh_access();
    }

    fn fail(&mut self, failure: Failure) {
        self.end_stream();
        self.link = match failure {
            Failure::Dropped(_) if self.retries < RETRIES => {
                let wait = FIRST_RETRY
                    .saturating_mul(1 << self.retries)
                    .min(LAST_RETRY);
                self.retries += 1;
                Link::Waiting(Instant::now() + wait)
            }
            Failure::Dropped(reason) => Link::Stopped(note(&format!(
                "t3term lost the terminal's connection and couldn't attach again in {RETRIES} tries. {reason}"
            ))),
            Failure::Missing => Link::Missing(None),
            Failure::Stopped(reason) => Link::Stopped(reason),
        };
    }

    /// Applies every event of `batch` in order, then accepts it, so T3 sends the next chunk
    /// only after the emulator has this one. Replies to the program's queries go out after.
    fn apply(&mut self, batch: TerminalBatch) {
        for event in batch.events() {
            self.apply_event(event);
        }
        batch.accept();
        self.queue_replies();
    }

    fn apply_event(&mut self, event: TerminalEvent<'_>) {
        match event {
            TerminalEvent::Snapshot(snapshot) => self.begin(&snapshot, None),
            TerminalEvent::Restarted { base, snapshot } => {
                if self.ours(&base) {
                    self.begin(&snapshot, base.sequence);
                }
            }
            TerminalEvent::Output { base, data } => {
                if self.fresh(&base) {
                    self.emulator.apply(data.as_str());
                }
            }
            // T3 emptied the history and left the shell running (`clear` in Manager.ts), as
            // the desktop empties its screen.
            TerminalEvent::Cleared { base } => {
                if self.fresh(&base) {
                    self.emulator.clear();
                }
            }
            TerminalEvent::Exited {
                base,
                exit_code,
                exit_signal,
            } => {
                if self.fresh(&base) {
                    self.set_status(Status::Exited {
                        code: exit_code,
                        signal: exit_signal,
                    });
                }
            }
            TerminalEvent::Error { base, message } => {
                if self.fresh(&base) {
                    self.set_status(Status::Failed(Some(note(message))));
                }
            }
            TerminalEvent::Activity { base, label, .. } => {
                if self.fresh(&base) {
                    self.shell.label = label::cut(label, LABEL);
                }
            }
            TerminalEvent::Closed { base } => {
                if self.fresh(&base) {
                    self.set_status(Status::Closed);
                }
            }
            // An event this t3term doesn't know changes nothing.
            TerminalEvent::Unknown(_) => {}
        }
    }

    /// A snapshot of this terminal: a new screen from its history, a new count of its events,
    /// and its state. Input, replies, a write or a resize meant for the screen before go
    /// nowhere. The shell gets the drawn size at once, since T3 sends none.
    fn begin(&mut self, snapshot: &SessionSnapshot<'_>, sequence: Option<u64>) {
        if snapshot.thread_id != self.id.thread_id()
            || snapshot.terminal_id != self.id.terminal_id()
        {
            return;
        }
        self.synced = true;
        self.seen = true;
        self.retries = 0;
        self.sequence = snapshot.sequence.or(sequence);
        self.shell = Shell {
            status: match snapshot.status {
                SessionStatus::Starting => Status::Starting,
                SessionStatus::Running => Status::Running,
                SessionStatus::Exited => Status::Exited {
                    code: snapshot.exit_code,
                    signal: snapshot.exit_signal,
                },
                SessionStatus::Error => Status::Failed(None),
            },
            label: label::cut(snapshot.label, LABEL),
        };
        self.input.clear();
        self.writing = None;
        self.resizing = None;
        self.input_failed = false;
        self.emulator.reset(snapshot.history.as_str());
        self.refresh_access();
        self.sent = None;
        self.resize_at = Some(Instant::now());
    }

    /// Whether `base` names this terminal.
    fn ours(&self, base: &EventBase<'_>) -> bool {
        base.thread_id == self.id.thread_id() && base.terminal_id == self.id.terminal_id()
    }

    /// Whether an event on `base` applies: it names this terminal, follows the stream's
    /// snapshot, and is newer than the last event applied when both have a sequence.
    fn fresh(&mut self, base: &EventBase<'_>) -> bool {
        if !self.synced || !self.ours(base) {
            return false;
        }
        match (base.sequence, self.sequence) {
            (Some(sequence), Some(last)) if sequence <= last => false,
            (Some(sequence), _) => {
                self.sequence = Some(sequence);
                true
            }
            (None, _) => true,
        }
    }

    fn set_status(&mut self, status: Status) {
        self.shell.status = status;
        self.refresh_access();
    }

    /// Whether input goes to the shell: the stream is open and synced, the shell runs, and no
    /// write has failed since the snapshot.
    fn writable(&self) -> bool {
        self.resizable() && !self.input_failed
    }

    /// Whether the shell takes a size: the stream is open and synced, and the shell runs.
    fn resizable(&self) -> bool {
        matches!(self.link, Link::Open)
            && self.synced
            && self.terminal.is_some()
            && matches!(self.shell.status, Status::Running)
    }

    /// Gives the emulator write access while input goes to the shell, so it queues replies to
    /// live output only, and drops the input waiting when it doesn't.
    fn refresh_access(&mut self) {
        let writable = self.writable();
        self.emulator.set_write_access(writable);
        if !writable {
            self.input.clear();
        }
    }

    fn queue_replies(&mut self) {
        // The emulator queues replies only while it has write access. A reply the queue has no
        // room for is dropped, as the emulator drops one its own queue has no room for.
        if let Some(replies) = self.emulator.take_replies() {
            self.input.push(&replies);
        }
    }

    /// Queues `text` for the shell, or says why it didn't.
    fn send(&mut self, text: &str, what: &str) {
        if !self.writable() {
            self.notice = Some(self.read_only().to_string());
            return;
        }
        if !self.input.push(text) {
            self.notice = Some(format!(
                "t3term didn't send that {what}: the terminal's input is still on its way. Try again in a moment."
            ));
            return;
        }
        // Typing brings the view back to the prompt.
        let offset = self.emulator.viewport_offset();
        if offset > 0 {
            self.scroll(-i32::try_from(offset).unwrap_or(i32::MAX));
        }
        self.pump();
    }

    /// Why input doesn't go to the shell now.
    fn read_only(&self) -> &'static str {
        match &self.link {
            Link::Attaching => "The terminal is still attaching.",
            Link::Open if !self.synced => "The terminal is still attaching.",
            Link::Open => match self.shell.status {
                Status::Starting => "The shell is still starting.",
                Status::Running => "The terminal's input failed. Press F4 twice to attach again.",
                Status::Exited { .. } => "The shell exited, so this terminal is read-only.",
                Status::Failed(_) => "The shell failed, so this terminal is read-only.",
                Status::Closed => "T3 closed this terminal, so it is read-only.",
            },
            Link::Missing(_) => "This thread has no terminal yet. Enter starts one.",
            Link::Waiting(_) => "The terminal is reconnecting.",
            Link::Stopped(_) => "The terminal stopped. Enter attaches again.",
        }
    }

    /// Starts the write and the resize the drawer can start: everything queued, if no write is
    /// on its way, and the drawn size once it is due, if no resize is.
    fn pump(&mut self) {
        if self.writing.is_none()
            && self.writable()
            && let Some(terminal) = self.terminal.clone()
            && let Some(text) = self.input.take()
        {
            self.writing = Some(async move { terminal.write(&text).await }.boxed_local());
        }
        let due = self.resize_at.is_some_and(|at| at <= Instant::now());
        if due && self.resizing.is_none() && self.resizable() {
            self.resize_at = None;
            if let Some((columns, rows)) = self.drawn.filter(|drawn| Some(*drawn) != self.sent)
                && let Some(terminal) = self.terminal.clone()
                && let Ok(size) = Size::new(columns, rows)
            {
                self.sent = Some((columns, rows));
                self.resizing = Some(async move { terminal.resize(size).await }.boxed_local());
            }
        }
    }

    fn resized(&mut self, result: Result<(), RpcError>) {
        match result {
            Ok(()) => {}
            // The stream sees the connection drop too, and attaches again.
            Err(RpcError::Disconnected(_)) => {}
            // The size counts as sent, so a resize T3 refuses isn't sent again until the size
            // changes.
            Err(error) => {
                self.notice = Some(note(&format!("T3 didn't resize the terminal: {error}")));
            }
        }
        // A size drawn while the resize was on its way goes now.
        if self.drawn != self.sent && self.resize_at.is_none() {
            self.resize_at = Some(Instant::now());
        }
    }

    /// The deadline that came: a synchronized update to apply, or an attach to try again. A
    /// resize that is due starts in `pump`.
    fn due(&mut self) -> bool {
        let now = Instant::now();
        let flushed = self.emulator.flush_sync_if_due(now.into_std());
        if flushed {
            self.queue_replies();
        }
        if let Link::Waiting(at) = self.link
            && at <= now
        {
            self.attach(None);
            return true;
        }
        flushed
    }

    /// The soonest deadline with work waiting on it, if any: a resize only while the shell
    /// takes one and hasn't the drawn size.
    fn deadline(&self) -> Option<Instant> {
        let sync = self.emulator.sync_deadline().map(Instant::from_std);
        let retry = match self.link {
            Link::Waiting(at) => Some(at),
            _ => None,
        };
        let unsent = self.drawn.is_some() && self.drawn != self.sent;
        let resize = self
            .resize_at
            .filter(|_| unsent && self.resizing.is_none() && self.resizable());
        [sync, retry, resize].into_iter().flatten().min()
    }
}

/// What the future in `slot` ends with, which empties the slot. Never, for an empty slot.
async fn finish<T>(slot: &mut Option<LocalBoxFuture<'static, T>>) -> T {
    let Some(future) = slot.as_mut() else {
        return std::future::pending().await;
    };
    let output = future.await;
    *slot = None;
    output
}

async fn next_batch(
    stream: &mut Option<TerminalStream>,
) -> Option<Result<TerminalBatch, RpcError>> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

/// Waits until `deadline`. One that has passed is ready on the first poll, which a timer
/// doesn't promise.
async fn sleep(deadline: Option<Instant>) {
    match deadline {
        Some(at) if at <= Instant::now() => {}
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// How an error from an attach or its stream ends it. Only a dropped connection is worth
/// another attach.
fn failure(error: &RpcError) -> Failure {
    match error {
        RpcError::Disconnected(_) => Failure::Dropped(note(&error.to_string())),
        RpcError::Failed {
            error_tag: Some(tag),
            ..
        } if tag == "TerminalSessionLookupError" => Failure::Missing,
        _ => Failure::Stopped(note(&error.to_string())),
    }
}

/// How a failure to open the terminal's connection ends an attach. A server that went away is
/// worth another attach. A refusal, a limit or a timeout isn't.
fn connect_failure(error: &anyhow::Error) -> Failure {
    let reason = note(&error.to_string());
    match error.downcast_ref::<T3Error>().map(|error| error.code) {
        Some("T3_DISCONNECTED" | "T3_SERVER_UNAVAILABLE") => Failure::Dropped(reason),
        _ => Failure::Stopped(reason),
    }
}

/// `text` cleaned of controls and cut to `NOTE` columns, as T3 or the connection wrote it.
fn note(text: &str) -> String {
    label::cut(text, NOTE)
}
