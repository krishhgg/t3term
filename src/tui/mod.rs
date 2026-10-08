//! The interactive client. It draws only after input or a server event, at most 30 times a second.
//! The only timer is a one-second tick, armed while a run is active, so the elapsed-time labels
//! and spinner advance. An idle TUI uses no CPU.

mod composer;
mod markdown;
mod picker;
mod theme;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use crate::client::{Client, IfBusy, WatchEvent};
use crate::models::{self, Choice, Plan};
use crate::projection::{Applied, ShellState, ThreadState, is_active_status, status};
use crate::transcript::{self, BlockKind};
use composer::Composer;
use markdown::Styles;
use picker::{Item, Kind, Pick};
use theme::{
    Theme, duration_label, model_display_name, monogram, now_ms, parse_iso_ms, relative_time,
    runtime_mode_label,
};

const FRAME: Duration = Duration::from_millis(33);
/// While a run is active, elapsed-time labels advance once a second.
const TICK: Duration = Duration::from_secs(1);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Text rows the composer grows to before it scrolls.
const COMPOSER_MAX_ROWS: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Transcript,
    Composer,
}

enum Row {
    /// A shelf divider such as `Settled (3)`.
    Section(String),
    Thread(String),
}

impl Row {
    /// Rows on screen: a card is three lines and a gap, a divider a gap and a line.
    fn height(&self) -> usize {
        match self {
            Row::Section(_) => 2,
            Row::Thread(_) => 4,
        }
    }
}

/// Wrapped lines for one transcript block, valid for one width and content version.
struct Cached {
    key: (u64, u16),
    lines: Vec<Line<'static>>,
}

enum ActionResult {
    Info(String),
    Error(String),
    /// T3's provider and model list, from `server.getConfig`.
    Config(Value),
    Sent {
        note: String,
    },
    /// A send that failed. The composer gets its text back.
    SendFailed {
        thread_id: String,
        message_id: String,
        text: String,
        error: String,
        /// One-message draft options this send took, such as ultrathink, to put back.
        reserved: Vec<(String, String)>,
    },
}

/// An open model, effort or mode menu.
struct Picker {
    kind: Kind,
    /// Search text. Only the model menu takes it.
    filter: String,
    /// Index into the menu's items, always an entry.
    selected: usize,
    /// First item shown.
    offset: usize,
}

struct OpenThread {
    id: String,
    state: Option<ThreadState>,
    events: mpsc::UnboundedReceiver<WatchEvent>,
    connection: String,
    /// Answers collected so far for the pending question request.
    answers: serde_json::Map<String, Value>,
    /// The event sequence of each run's last change, which keys the clock row under its prompt.
    run_changes: HashMap<String, u64>,
}

/// A message whose send failed. If the send timed out, T3 may still have it.
struct Unconfirmed {
    thread_id: String,
    message_id: String,
    text: String,
}

struct App {
    client: Arc<Client>,
    theme: Theme,
    config: Option<Value>,
    /// Model, effort and mode choices per thread, sent with the thread's next message.
    drafts: HashMap<String, Choice>,
    /// Messages that failed to send while the composer held other text, by thread.
    unsent: HashMap<String, Vec<String>>,
    /// Failed sends to look for in their thread, in case they reached T3 after all.
    unconfirmed: Vec<Unconfirmed>,
    picker: Option<Picker>,
    shell: Option<ShellState>,
    shell_connection: String,
    rows: Vec<Row>,
    selected: usize,
    sidebar_offset: usize,
    open: Option<OpenThread>,
    focus: Focus,
    composer: Composer,
    /// Rows scrolled up from the bottom. Zero follows new output.
    scroll: usize,
    /// Show tool rows for finished runs and their output.
    expand_tools: bool,
    /// Settled threads stay behind the `Settled (N)` shelf until it is opened.
    show_settled: bool,
    settled_count: usize,
    cache: HashMap<String, Cached>,
    message: Option<(String, bool)>,
    actions: mpsc::UnboundedSender<ActionResult>,
    /// Wall-clock time of the frame being drawn, in milliseconds.
    now: i64,
    // Last drawn geometry, for mouse hit-testing and page sizes.
    sidebar_list: Rect,
    sidebar_footer: Rect,
    transcript_area: Rect,
    panel_area: Rect,
    /// Rows the request panel's text is scrolled down, and the request that applies to.
    panel_scroll: usize,
    panel_key: String,
    composer_area: Rect,
    /// Each composer chip and the menu it opens.
    chips: Vec<(Rect, Kind)>,
    picker_area: Rect,
    /// The screen row of each menu item drawn, with its index.
    picker_rows: Vec<(u16, usize)>,
    quit: bool,
}

pub async fn run(client: Arc<Client>) -> Result<()> {
    let mut screen = Screen(Some(ratatui::init()));
    crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let Some(terminal) = screen.0.as_mut() else {
        return Ok(());
    };
    event_loop(terminal, client).await
}

/// Owns the terminal while the TUI runs and puts it back when dropped, including when main
/// drops the TUI after a signal.
struct Screen(Option<ratatui::DefaultTerminal>);

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
        // Once the terminal has closed, ratatui's own Drop for Terminal and `ratatui::restore`
        // print their errors with `eprintln!`, which panics. A panic here aborts the process
        // before main revokes the session, so skip both when the terminal is gone.
        if let Some(mut terminal) = self.0.take()
            && terminal.show_cursor().is_err()
        {
            std::mem::forget(terminal);
        }
        let _ = ratatui::try_restore();
    }
}

async fn event_loop(terminal: &mut ratatui::DefaultTerminal, client: Arc<Client>) -> Result<()> {
    let (actions, mut action_results) = mpsc::unbounded_channel();
    let mut shell_events = client.watch_shell(None);
    let mut app = App {
        client,
        theme: Theme::detect(),
        config: None,
        drafts: HashMap::new(),
        unsent: HashMap::new(),
        unconfirmed: Vec::new(),
        picker: None,
        shell: None,
        shell_connection: "connecting".into(),
        rows: Vec::new(),
        selected: 0,
        sidebar_offset: 0,
        open: None,
        focus: Focus::Sidebar,
        composer: Composer::default(),
        scroll: 0,
        expand_tools: false,
        show_settled: false,
        settled_count: 0,
        cache: HashMap::new(),
        message: None,
        actions,
        now: now_ms(),
        sidebar_list: Rect::default(),
        sidebar_footer: Rect::default(),
        transcript_area: Rect::default(),
        panel_area: Rect::default(),
        panel_scroll: 0,
        panel_key: String::new(),
        composer_area: Rect::default(),
        chips: Vec::new(),
        picker_area: Rect::default(),
        picker_rows: Vec::new(),
        quit: false,
    };
    app.load_config();
    let mut input = EventStream::new();
    let mut dirty = true;
    let mut last_draw = Instant::now() - FRAME;
    let mut tick_at = Instant::now() + TICK;

    while !app.quit {
        if dirty && last_draw.elapsed() >= FRAME {
            terminal.draw(|frame| app.draw(frame))?;
            last_draw = Instant::now();
            dirty = false;
        }
        let redraw_at = tokio::time::Instant::from_std(last_draw + FRAME);
        let running = app.running();
        if !running {
            tick_at = Instant::now() + TICK;
        }
        let tick_deadline = tokio::time::Instant::from_std(tick_at);
        let thread_event = async {
            match app.open.as_mut() {
                Some(open) => open.events.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            event = input.next() => match event {
                Some(Ok(event)) => dirty |= app.on_terminal_event(event),
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            Some(event) = shell_events.recv() => {
                app.on_shell_event(event);
                dirty = true;
            }
            Some(event) = thread_event => {
                app.on_thread_event(event);
                dirty = true;
            }
            Some(result) = action_results.recv() => {
                app.on_action_result(result);
                dirty = true;
            }
            // Only armed while a draw is waiting out the frame budget.
            _ = tokio::time::sleep_until(redraw_at), if dirty => {}
            // Only armed while a run is active, to advance the clocks and spinner.
            _ = tokio::time::sleep_until(tick_deadline), if running => {
                tick_at = Instant::now() + TICK;
                dirty = true;
            }
        }
    }
    Ok(())
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn hash(parts: &[&str], stamp: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut hasher);
    stamp.hash(&mut hasher);
    hasher.finish()
}

impl App {
    // ---- server events ----

    fn on_shell_event(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Item(item) => {
                self.shell
                    .get_or_insert_with(ShellState::default)
                    .apply(&item);
                if item["kind"] == "synchronized" {
                    self.shell_connection = "live".into();
                }
                self.rebuild_rows();
                self.settle_draft();
            }
            WatchEvent::Reconnecting { .. } => self.shell_connection = "reconnecting".into(),
            WatchEvent::Failed(message) => self.message = Some((message, true)),
        }
    }

    fn on_thread_event(&mut self, event: WatchEvent) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        match event {
            WatchEvent::Item(item) => {
                let state = open.state.get_or_insert_with(ThreadState::default);
                match state.apply(&item) {
                    Applied::Synchronized => open.connection = "live".into(),
                    Applied::Snapshot => self.cache.clear(),
                    Applied::Event(kind) if kind.starts_with("run.") => {
                        if let Some(run_id) =
                            item.pointer("/event/payload/id").and_then(Value::as_str)
                        {
                            open.run_changes.insert(run_id.to_string(), state.sequence);
                        }
                    }
                    _ => {}
                }
                self.settle_draft();
                self.drop_landed();
            }
            WatchEvent::Reconnecting { reason, .. } => {
                open.connection = format!("reconnecting: {reason}")
            }
            WatchEvent::Failed(message) => {
                open.connection = "closed".into();
                self.message = Some((message, true));
            }
        }
    }

    /// Whether any thread is working, which is when the clocks need a tick.
    fn running(&self) -> bool {
        // A closed watch never hears that its run finished, so its last state can't count.
        if self
            .open
            .as_ref()
            .filter(|o| o.connection != "closed")
            .and_then(|o| o.state.as_ref())
            .is_some_and(|s| s.active_run().is_some())
        {
            return true;
        }
        self.shell
            .as_ref()
            .is_some_and(|s| s.threads.iter().any(|t| is_active_status(status(t))))
    }

    /// A flat list of cards newest first, like the GUI's default sidebar, with settled threads
    /// under their own divider. Delegated child threads are hidden.
    fn rebuild_rows(&mut self) {
        let Some(shell) = self.shell.as_ref() else {
            return;
        };
        let selected_id = self.selected_thread_id().map(str::to_string);
        let mut threads: Vec<&Value> = shell
            .threads
            .iter()
            .filter(|t| t["lineage"]["parentThreadId"].is_null())
            .collect();
        threads.sort_by(|a, b| str_of(b, "updatedAt").cmp(str_of(a, "updatedAt")));
        let (settled, active): (Vec<&Value>, Vec<&Value>) =
            threads.into_iter().partition(|t| !t["settledAt"].is_null());
        self.rows.clear();
        for thread in &active {
            self.rows
                .push(Row::Thread(str_of(thread, "id").to_string()));
        }
        self.settled_count = settled.len();
        if self.show_settled && !settled.is_empty() {
            self.rows
                .push(Row::Section(format!("Settled ({})", settled.len())));
            for thread in &settled {
                self.rows
                    .push(Row::Thread(str_of(thread, "id").to_string()));
            }
        }
        self.selected = selected_id
            .and_then(|id| {
                self.rows
                    .iter()
                    .position(|r| matches!(r, Row::Thread(t) if *t == id))
            })
            .or_else(|| self.rows.iter().position(|r| matches!(r, Row::Thread(_))))
            .unwrap_or(0);
    }

    fn toggle_settled(&mut self) {
        self.show_settled = !self.show_settled;
        self.rebuild_rows();
    }

    fn selected_thread_id(&self) -> Option<&str> {
        match self.rows.get(self.selected) {
            Some(Row::Thread(id)) => Some(id),
            _ => None,
        }
    }

    fn shell_thread(&self, id: &str) -> Option<&Value> {
        self.shell
            .as_ref()?
            .threads
            .iter()
            .find(|t| str_of(t, "id") == id)
    }

    fn project_title(&self, project_id: &str) -> String {
        self.shell
            .as_ref()
            .and_then(|s| s.projects.iter().find(|p| str_of(p, "id") == project_id))
            .map(|p| str_of(p, "title").to_string())
            .unwrap_or_else(|| "Project".into())
    }

    fn move_selection(&mut self, delta: isize) {
        let mut index = self.selected as isize;
        loop {
            index += delta;
            if index < 0 || index >= self.rows.len() as isize {
                return;
            }
            if matches!(self.rows[index as usize], Row::Thread(_)) {
                self.selected = index as usize;
                return;
            }
        }
    }

    fn open_selected(&mut self) {
        let Some(id) = self.selected_thread_id().map(str::to_string) else {
            return;
        };
        if self.open.as_ref().is_some_and(|o| o.id == id) {
            self.focus = Focus::Composer;
            return;
        }
        let events = self.client.watch_thread(&id, None);
        self.open = Some(OpenThread {
            id,
            state: None,
            events,
            connection: "connecting".into(),
            answers: Default::default(),
            run_changes: HashMap::new(),
        });
        self.cache.clear();
        self.scroll = 0;
        self.picker = None;
        self.focus = Focus::Composer;
        if self.composer.is_empty()
            && let Some(texts) = self.open.as_ref().and_then(|o| self.unsent.remove(&o.id))
        {
            self.composer.insert_str(&texts.join("\n\n"));
        }
    }

    // ---- input ----

    fn on_terminal_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.on_key(key);
                true
            }
            Event::Paste(text) => {
                if self.focus == Focus::Composer {
                    self.composer.insert_str(&text);
                }
                true
            }
            Event::Mouse(mouse) => {
                let inside = |area: Rect| {
                    mouse.column >= area.x
                        && mouse.column < area.x + area.width
                        && mouse.row >= area.y
                        && mouse.row < area.y + area.height
                };
                let click = mouse.kind == MouseEventKind::Down(MouseButton::Left);
                let chip = self
                    .chips
                    .iter()
                    .find(|(area, _)| inside(*area))
                    .map(|(_, kind)| *kind);
                if let Some(open) = self.picker.as_ref() {
                    let open_kind = open.kind;
                    match mouse.kind {
                        _ if click && inside(self.picker_area) => {
                            if let Some(&(_, index)) =
                                self.picker_rows.iter().find(|(y, _)| *y == mouse.row)
                            {
                                self.choose(index);
                            }
                        }
                        // A click elsewhere closes the menu. On another chip it opens that one.
                        _ if click => {
                            self.picker = None;
                            if let Some(kind) = chip.filter(|kind| *kind != open_kind) {
                                self.open_picker(kind);
                            }
                        }
                        MouseEventKind::ScrollUp if inside(self.picker_area) => {
                            self.move_picker(-1)
                        }
                        MouseEventKind::ScrollDown if inside(self.picker_area) => {
                            self.move_picker(1)
                        }
                        _ => return false,
                    }
                    return true;
                }
                if let Some(kind) = chip.filter(|_| click) {
                    self.focus = Focus::Composer;
                    self.open_picker(kind);
                    return true;
                }
                match mouse.kind {
                    MouseEventKind::ScrollUp if inside(self.panel_area) => {
                        self.panel_scroll = self.panel_scroll.saturating_sub(1)
                    }
                    MouseEventKind::ScrollDown if inside(self.panel_area) => self.panel_scroll += 1,
                    MouseEventKind::ScrollUp if inside(self.transcript_area) => self.scroll += 3,
                    MouseEventKind::ScrollDown if inside(self.transcript_area) => {
                        self.scroll = self.scroll.saturating_sub(3)
                    }
                    MouseEventKind::ScrollUp if inside(self.sidebar_list) => {
                        self.move_selection(-1)
                    }
                    MouseEventKind::ScrollDown if inside(self.sidebar_list) => {
                        self.move_selection(1)
                    }
                    MouseEventKind::Down(MouseButton::Left) if inside(self.sidebar_footer) => {
                        self.toggle_settled()
                    }
                    MouseEventKind::Down(MouseButton::Left) if inside(self.sidebar_list) => {
                        let y = (mouse.row - self.sidebar_list.y) as usize;
                        if let Some(row) = self.sidebar_row_at(y)
                            && matches!(self.rows.get(row), Some(Row::Thread(_)))
                        {
                            self.selected = row;
                            self.open_selected();
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) if inside(self.transcript_area) => {
                        self.focus = Focus::Transcript
                    }
                    MouseEventKind::Down(MouseButton::Left) => self.focus = Focus::Composer,
                    _ => return false,
                }
                true
            }
            Event::Resize(..) => {
                self.cache.clear();
                true
            }
            _ => false,
        }
    }

    /// The sidebar entry drawn at `y` rows below the top of the list.
    fn sidebar_row_at(&self, y: usize) -> Option<usize> {
        let mut top = 0;
        for (index, row) in self.rows.iter().enumerate().skip(self.sidebar_offset) {
            if y < top + row.height() {
                return Some(index);
            }
            top += row.height();
        }
        None
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Status messages are toasts: the next key dismisses them.
        self.message = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('q') if ctrl => {
                self.quit = true;
                return;
            }
            KeyCode::Char('x') if ctrl => {
                self.interrupt();
                return;
            }
            // Answer approvals from any pane. Terminals report Esc followed quickly by a letter as Alt.
            KeyCode::Char(c @ ('a' | 's' | 'd')) if alt => {
                self.respond(match c {
                    'a' => "accept",
                    's' => "acceptForSession",
                    _ => "decline",
                });
                return;
            }
            // Scroll a request panel too long to show at once.
            KeyCode::Up if alt && self.panel_area.height > 0 => {
                self.panel_scroll = self.panel_scroll.saturating_sub(1);
                return;
            }
            KeyCode::Down if alt && self.panel_area.height > 0 => {
                self.panel_scroll += 1;
                return;
            }
            _ if self.picker.is_some() => {
                self.on_picker_key(key);
                return;
            }
            // The desktop app's model, effort and mode menus. It uses Cmd+Shift+M, E and A.
            KeyCode::Char(c @ ('m' | 'e' | 'p')) if alt => {
                self.open_picker(match c {
                    'm' => Kind::Model,
                    'e' => Kind::Traits,
                    _ => Kind::Mode,
                });
                return;
            }
            KeyCode::Char('r') if ctrl => {
                self.swap_unsent();
                return;
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Composer,
                    Focus::Composer => Focus::Transcript,
                    Focus::Transcript => Focus::Sidebar,
                };
                return;
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Transcript,
                    Focus::Composer => Focus::Sidebar,
                    Focus::Transcript => Focus::Composer,
                };
                return;
            }
            KeyCode::PageUp => {
                self.scroll += self.page();
                return;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(self.page());
                return;
            }
            _ => {}
        }
        match self.focus {
            Focus::Composer => match key.code {
                KeyCode::Esc => self.focus = Focus::Transcript,
                KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.composer.insert('\n')
                }
                KeyCode::Char('j') if ctrl => self.composer.insert('\n'),
                KeyCode::Enter => self.submit(),
                KeyCode::Char('w') if ctrl => self.composer.delete_word(),
                KeyCode::Char('u') if ctrl => self.composer.clear(),
                KeyCode::Char('a') if ctrl => self.composer.home(),
                KeyCode::Char('e') if ctrl => self.composer.end(),
                KeyCode::Char(c) if !ctrl && !alt => self.composer.insert(c),
                KeyCode::Backspace => self.composer.backspace(),
                KeyCode::Delete => self.composer.delete(),
                KeyCode::Left => self.composer.left(),
                KeyCode::Right => self.composer.right(),
                KeyCode::Home => self.composer.home(),
                KeyCode::End => self.composer.end(),
                KeyCode::Up => {
                    // Past the first line, the arrow scrolls the transcript instead.
                    let moved = self.composer.up();
                    if !moved {
                        self.scroll += 1;
                    }
                }
                KeyCode::Down => {
                    let moved = self.composer.down();
                    if !moved {
                        self.scroll = self.scroll.saturating_sub(1);
                    }
                }
                _ => {}
            },
            Focus::Sidebar => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected(),
                KeyCode::Char('e') => self.toggle_settled(),
                _ => {}
            },
            Focus::Transcript => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Up | KeyCode::Char('k') => self.scroll += 1,
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Char('g') | KeyCode::Home => self.scroll = usize::MAX / 2,
                KeyCode::Char('G') | KeyCode::End => self.scroll = 0,
                KeyCode::Char('t') => {
                    self.expand_tools = !self.expand_tools;
                    self.cache.clear();
                }
                KeyCode::Char('a') => self.respond("accept"),
                KeyCode::Char('s') => self.respond("acceptForSession"),
                KeyCode::Char('d') => self.respond("decline"),
                KeyCode::Char('i') | KeyCode::Enter => self.focus = Focus::Composer,
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Sidebar,
                _ => {}
            },
        }
    }

    fn page(&self) -> usize {
        (self.transcript_area.height as usize)
            .saturating_sub(2)
            .max(1)
    }

    fn pending_request(&self) -> Option<(Value, Option<Value>)> {
        let state = self.open.as_ref()?.state.as_ref()?;
        let request = state.pending_requests().into_iter().next()?.clone();
        let item = state.request_item(str_of(&request, "id")).cloned();
        Some((request, item))
    }

    fn spawn_action<F>(&self, action: F)
    where
        F: std::future::Future<Output = Result<String>> + Send + 'static,
    {
        let results = self.actions.clone();
        tokio::spawn(async move {
            let _ = results.send(match action.await {
                Ok(text) => ActionResult::Info(text),
                Err(e) => ActionResult::Error(e.to_string()),
            });
        });
    }

    fn on_action_result(&mut self, result: ActionResult) {
        match result {
            ActionResult::Info(text) => self.message = Some((text, false)),
            ActionResult::Error(text) => self.message = Some((text, true)),
            ActionResult::Config(config) => {
                // A refresh can add or drop rows, so the highlight follows its entry, not
                // its row number.
                let previous = self.selected_pick();
                self.config = Some(config);
                if self.picker.is_some() {
                    self.select_pick(previous);
                }
            }
            ActionResult::Sent { note } => {
                self.settle_draft();
                self.message = Some((note, false));
            }
            ActionResult::SendFailed {
                thread_id,
                message_id,
                text,
                error,
                reserved,
            } => {
                // The one-message choice didn't go out, so it waits for the next try, unless
                // the user has picked another value for it since.
                if !reserved.is_empty() {
                    let draft = self.drafts.entry(thread_id.clone()).or_default();
                    for (id, value) in reserved {
                        if !draft.options.iter().any(|(other, _)| *other == id) {
                            draft.options.push((id, value));
                        }
                    }
                }
                self.unconfirmed.push(Unconfirmed {
                    thread_id: thread_id.clone(),
                    message_id,
                    text: text.clone(),
                });
                self.give_back(&thread_id, &text, error);
            }
        }
    }

    /// A send that timed out can still reach T3. Once the open thread shows one, the copy kept
    /// for a retry goes, so it isn't sent twice.
    fn drop_landed(&mut self) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let Some(state) = open.state.as_ref() else {
            return;
        };
        let (landed, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.unconfirmed)
            .into_iter()
            .partition(|m| m.thread_id == open.id && state.has_message(&m.message_id));
        self.unconfirmed = waiting;
        for message in landed {
            let saved = self.unsent.get_mut(&message.thread_id);
            let removed = if let Some(texts) = saved
                && let Some(index) = texts.iter().position(|t| *t == message.text)
            {
                texts.remove(index);
                if texts.is_empty() {
                    self.unsent.remove(&message.thread_id);
                }
                "dropped its saved copy"
            } else if self.composer.text() == message.text {
                self.composer.clear();
                "cleared it from the composer"
            } else {
                continue;
            };
            self.message = Some((
                format!("The message that failed reached T3 after all, so t3term {removed}."),
                false,
            ));
        }
    }

    /// Puts an unsent message back in the composer. If the composer holds other text, or the
    /// message was for another thread, it waits in `unsent` for Ctrl+R or for that thread.
    fn give_back(&mut self, thread_id: &str, text: &str, error: String) {
        if self.open.as_ref().is_some_and(|o| o.id == thread_id) && self.composer.is_empty() {
            self.composer.insert_str(text);
            self.message = Some((error, true));
            return;
        }
        self.unsent
            .entry(thread_id.to_string())
            .or_default()
            .push(text.to_string());
        self.message = Some((
            format!(
                "{}. The message is saved. Ctrl+R in its thread brings it back.",
                error.trim_end().trim_end_matches('.')
            ),
            true,
        ));
    }

    /// Swaps the composer's text with the open thread's unsent message, so neither is lost.
    fn swap_unsent(&mut self) {
        let Some(id) = self.open.as_ref().map(|o| o.id.clone()) else {
            return;
        };
        let Some(unsent) = self.unsent.remove(&id) else {
            self.message = Some(("This thread has no unsent message.".into(), false));
            return;
        };
        let current = self.composer.text();
        self.composer.clear();
        self.composer.insert_str(&unsent.join("\n\n"));
        if !current.trim().is_empty() {
            self.unsent.insert(id, vec![current]);
        }
        self.focus = Focus::Composer;
    }

    /// Drops the open thread's draft once the thread has every choice in it, so the next
    /// message doesn't undo a change made in another client.
    fn settle_draft(&mut self) {
        let (Some(open), Some(config)) = (self.open.as_ref(), self.config.as_ref()) else {
            return;
        };
        let (Some(draft), Some(thread)) = (self.drafts.get(&open.id), self.open_thread_json())
        else {
            return;
        };
        if models::plan(config, thread, draft).is_ok_and(|plan| plan == Plan::default()) {
            let id = open.id.clone();
            self.drafts.remove(&id);
        }
    }

    // ---- model, effort and mode menus ----

    /// Fetches T3's provider list in the background. Menus use the last copy until it arrives.
    fn load_config(&self) {
        let (client, results) = (self.client.clone(), self.actions.clone());
        tokio::spawn(async move {
            let _ = results.send(match client.server_config().await {
                Ok(config) => ActionResult::Config(config),
                Err(e) => ActionResult::Error(format!("Couldn't read T3's model list: {e}")),
            });
        });
    }

    /// The open thread's settings with its draft applied.
    fn settings_view(&self) -> Option<picker::View> {
        let open = self.open.as_ref()?;
        let thread = self.open_thread_json()?;
        let empty = Choice::default();
        let draft = self.drafts.get(&open.id).unwrap_or(&empty);
        Some(picker::view(self.config.as_ref(), thread, draft))
    }

    fn picker_items(&self) -> Vec<Item> {
        let (Some(picker), Some(config), Some(view)) = (
            self.picker.as_ref(),
            self.config.as_ref(),
            self.settings_view(),
        ) else {
            return Vec::new();
        };
        picker::items(picker.kind, config, &view, &picker.filter)
    }

    fn open_picker(&mut self, kind: Kind) {
        if self.open_thread_json().is_none() {
            self.message = Some((
                "Open a thread first: pick one in the sidebar and press Enter.".into(),
                true,
            ));
            return;
        }
        // Provider status changes while T3 runs, so each menu refreshes the list.
        if self.picker.is_none() {
            self.load_config();
        }
        self.focus = Focus::Composer;
        self.picker = Some(Picker {
            kind,
            filter: String::new(),
            selected: 0,
            offset: 0,
        });
        self.select_pick(None);
    }

    /// The pick under the menu's highlight.
    fn selected_pick(&self) -> Option<Pick> {
        let selected = self.picker.as_ref()?.selected;
        match self.picker_items().get(selected)? {
            Item::Entry { pick, .. } => Some(pick.clone()),
            Item::Heading { .. } => None,
        }
    }

    /// Highlights `pick` if the menu still has it, else the current choice, else the first
    /// entry.
    fn select_pick(&mut self, pick: Option<Pick>) {
        let items = self.picker_items();
        let index = pick
            .and_then(|pick| {
                items
                    .iter()
                    .position(|i| matches!(i, Item::Entry { pick: p, .. } if *p == pick))
            })
            .or_else(|| {
                items
                    .iter()
                    .position(|i| matches!(i, Item::Entry { current: true, .. }))
            })
            .or_else(|| items.iter().position(Item::is_entry));
        if let Some(picker) = self.picker.as_mut() {
            picker.selected = index.unwrap_or(0);
        }
    }

    fn on_picker_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let Some((kind, selected)) = self.picker.as_ref().map(|p| (p.kind, p.selected)) else {
            return;
        };
        let searching = kind == Kind::Model;
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Enter => self.choose(selected),
            KeyCode::Up => self.move_picker(-1),
            KeyCode::Down => self.move_picker(1),
            KeyCode::Char('p') if ctrl => self.move_picker(-1),
            KeyCode::Char('n') if ctrl => self.move_picker(1),
            KeyCode::PageUp => self.move_picker(-8),
            KeyCode::PageDown => self.move_picker(8),
            KeyCode::Home => self.move_picker(-isize::MAX),
            KeyCode::End => self.move_picker(isize::MAX),
            // The same shortcut closes the menu. Another one switches to its menu.
            KeyCode::Char(c @ ('m' | 'e' | 'p')) if alt => {
                let next = match c {
                    'm' => Kind::Model,
                    'e' => Kind::Traits,
                    _ => Kind::Mode,
                };
                if next == kind {
                    self.picker = None;
                } else {
                    self.open_picker(next);
                }
            }
            KeyCode::Char('k') if !searching => self.move_picker(-1),
            KeyCode::Char('j') if !searching => self.move_picker(1),
            KeyCode::Char(c) if searching && !ctrl && !alt => self.set_filter(|f| f.push(c)),
            KeyCode::Backspace if searching => self.set_filter(|f| {
                f.pop();
            }),
            _ => {}
        }
    }

    fn set_filter(&mut self, edit: impl FnOnce(&mut String)) {
        if let Some(picker) = self.picker.as_mut() {
            edit(&mut picker.filter);
            picker.offset = 0;
        }
        let first = self.picker_items().iter().position(Item::is_entry);
        if let Some(picker) = self.picker.as_mut() {
            picker.selected = first.unwrap_or(0);
        }
    }

    /// Moves the highlight by `delta` entries, skipping section labels.
    fn move_picker(&mut self, delta: isize) {
        let items = self.picker_items();
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let step = delta.signum();
        let mut index = picker.selected as isize;
        for _ in 0..delta.unsigned_abs() {
            let mut next = index + step;
            while items.get(next as usize).is_some_and(|i| !i.is_entry()) {
                next += step;
            }
            if next < 0 || next as usize >= items.len() {
                break;
            }
            index = next;
        }
        picker.selected = index as usize;
    }

    /// Records the entry at `index` in the open thread's draft and closes the menu.
    fn choose(&mut self, index: usize) {
        let items = self.picker_items();
        let Some(Item::Entry { pick, .. }) = items.get(index) else {
            return;
        };
        self.picker = None;
        let (Some(open), Some(config), Some(thread)) = (
            self.open.as_ref(),
            self.config.as_ref(),
            self.open_thread_json(),
        ) else {
            return;
        };
        let id = open.id.clone();
        let mut draft = self.drafts.get(&id).cloned().unwrap_or_default();
        picker::apply(&mut draft, thread, pick);
        // Check the draft now, so a choice T3 would refuse fails here rather than at send.
        match models::plan(config, thread, &draft) {
            Err(e) => self.message = Some((e.to_string(), true)),
            Ok(plan) if plan == Plan::default() => {
                self.drafts.remove(&id);
            }
            Ok(_) => {
                self.drafts.insert(id, draft);
            }
        }
    }

    fn submit(&mut self) {
        if self.composer.is_empty() {
            return;
        }
        let Some(open) = self.open.as_mut() else {
            self.message = Some((
                "Open a thread first: pick one in the sidebar and press Enter.".into(),
                true,
            ));
            return;
        };
        let Some(state) = open.state.clone() else {
            return;
        };
        let text = self.composer.text();

        // A pending question takes the composer's text as its answer, one question at a time.
        if let Some(request) = state
            .pending_requests()
            .into_iter()
            .find(|r| r["kind"] == "user_input")
        {
            let request_id = str_of(request, "id").to_string();
            let questions = state
                .request_item(&request_id)
                .and_then(|i| i["questions"].as_array().cloned())
                .unwrap_or_default();
            if let Some(question) = questions
                .iter()
                .find(|q| !open.answers.contains_key(str_of(q, "id")))
            {
                let options = question["options"].as_array().cloned().unwrap_or_default();
                let answer = match text.trim().parse::<usize>() {
                    Ok(n) if (1..=options.len()).contains(&n) => {
                        let option = &options[n - 1];
                        option
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or(str_of(option, "label"))
                            .to_string()
                    }
                    _ => text.trim().to_string(),
                };
                let answer = if question["multiSelect"] == true {
                    json!([answer])
                } else {
                    json!(answer)
                };
                open.answers
                    .insert(str_of(question, "id").to_string(), answer);
                self.composer.clear();
                if open.answers.len() < questions.len() {
                    return;
                }
                let answers = Value::Object(std::mem::take(&mut open.answers));
                let (client, thread_id) = (self.client.clone(), open.id.clone());
                self.spawn_action(async move {
                    client.answer(&thread_id, &request_id, answers).await?;
                    Ok("Answer sent.".into())
                });
                return;
            }
        }

        self.composer.clear();
        self.scroll = 0;
        // The draft goes out with this message and stays until the thread shows it.
        let thread_id = open.id.clone();
        let plan = match (self.config.as_ref(), self.drafts.get(&thread_id)) {
            (Some(config), Some(draft)) => match models::plan(config, state.thread(), draft) {
                Ok(plan) => plan,
                Err(e) => return self.give_back(&thread_id, &text, e.to_string()),
            },
            _ => Plan::default(),
        };
        // The client refuses this too, but its message names the run id.
        if plan.changes_modes() && state.active_run().is_some() {
            let error = "T3 can't change the mode during a run. Send again when it finishes, or set the mode back with Alt+P.";
            return self.give_back(&thread_id, &text, error.into());
        }
        // As in the desktop app, ultrathink applies to one message. This send takes it out of
        // the draft now, so a second message sent before this one lands doesn't reuse it.
        let mut reserved = Vec::new();
        if let Some(effort) = &plan.prompt_effort
            && let Some(draft) = self.drafts.get_mut(&thread_id)
        {
            let (taken, kept) = std::mem::take(&mut draft.options)
                .into_iter()
                .partition(|(_, value)| value == effort);
            draft.options = kept;
            reserved = taken;
            if draft.is_empty() {
                self.drafts.remove(&thread_id);
            }
        }
        let (client, results) = (self.client.clone(), self.actions.clone());
        let message_id = uuid::Uuid::new_v4().to_string();
        tokio::spawn(async move {
            let sent = client
                .send_message_as(&message_id, &state, &text, IfBusy::Queue, &plan)
                .await;
            let _ = results.send(match sent {
                Ok(receipt) => ActionResult::Sent {
                    note: match receipt.dispatch_mode {
                        "queue_after_active" => "Queued after the running turn.".into(),
                        _ => "Sent.".into(),
                    },
                },
                Err(e) => ActionResult::SendFailed {
                    thread_id,
                    message_id,
                    text,
                    error: e.to_string(),
                    reserved,
                },
            });
        });
    }

    fn respond(&mut self, decision: &'static str) {
        let Some((request, _)) = self.pending_request() else {
            return;
        };
        if request["kind"] == "user_input" {
            self.focus = Focus::Composer;
            return;
        }
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let (client, thread_id, request_id) = (
            self.client.clone(),
            open.id.clone(),
            str_of(&request, "id").to_string(),
        );
        self.spawn_action(async move {
            client.respond(&thread_id, &request_id, decision).await?;
            Ok(format!("Answered: {decision}."))
        });
    }

    fn interrupt(&mut self) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let Some(run_id) = open
            .state
            .as_ref()
            .and_then(|s| s.active_run())
            .map(|r| str_of(r, "id").to_string())
        else {
            self.message = Some(("Nothing is running.".into(), false));
            return;
        };
        let (client, thread_id) = (self.client.clone(), open.id.clone());
        self.spawn_action(async move {
            client.interrupt(&thread_id, &run_id).await?;
            Ok("Interrupt sent.".into())
        });
    }

    // ---- drawing ----

    fn spinner(&self) -> &'static str {
        SPINNER[(self.now / 1000).rem_euclid(SPINNER.len() as i64) as usize]
    }

    /// The thread record for the open thread: the shell's copy, else the projection's.
    fn open_thread_json(&self) -> Option<&Value> {
        let open = self.open.as_ref()?;
        self.shell_thread(&open.id)
            .or_else(|| open.state.as_ref().map(ThreadState::thread))
    }

    fn connection_state(&self) -> (String, Color) {
        let t = &self.theme;
        let connection = match self.open.as_ref() {
            Some(open) => open.connection.clone(),
            None => self.shell_connection.clone(),
        };
        let color = match connection.as_str() {
            "live" => t.success,
            "closed" => t.error,
            _ => t.warning_fg,
        };
        (connection, color)
    }

    fn draw(&mut self, frame: &mut Frame) {
        self.now = now_ms();
        let area = frame.area();
        frame.render_widget(Block::new().style(Style::new().bg(self.theme.bg)), area);
        let sidebar_width = (area.width / 4).clamp(26, 34).min(area.width / 2);
        let [sidebar, main] =
            Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(20)])
                .areas(area);
        self.draw_sidebar(frame, sidebar);

        // The main column keeps one blank column on each side, like the GUI's padding.
        let main = Rect {
            x: main.x + 1,
            width: main.width.saturating_sub(2),
            ..main
        };
        let (composer_rows, cursor) = self.composer.layout(main.width.saturating_sub(4) as usize);
        // Text rows, a spacer, the chips row and two borders.
        let composer_height = composer_rows.len().clamp(1, COMPOSER_MAX_ROWS) as u16 + 4;
        // The panel leaves the header, three transcript rows, the composer and the status line
        // their space, less its own two borders.
        let panel_width = main.width.saturating_sub(4) as usize;
        let panel_room = main.height.saturating_sub(2 + 3 + composer_height + 1 + 2);
        let panel = match self.request_panel(panel_width) {
            Some(panel) => self.panel_rows(panel, panel_width, panel_room as usize),
            None => Vec::new(),
        };
        let panel_height = if panel.is_empty() {
            0
        } else {
            panel.len() as u16 + 2
        };
        let [header, body, panel_area, composer_area, status_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(panel_height),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .areas(main);

        frame.render_widget(
            Paragraph::new(self.header_line(header.width as usize)),
            header,
        );
        self.transcript_area = body;
        self.draw_transcript(frame, body);
        self.panel_area = panel_area;
        if !panel.is_empty() {
            self.draw_request_panel(frame, panel_area, panel);
        }
        self.composer_area = composer_area;
        self.draw_composer(frame, composer_area, &composer_rows, cursor);
        self.picker_area = Rect::default();
        self.picker_rows.clear();
        if self.picker.is_some() {
            let above = Rect {
                height: composer_area.y.saturating_sub(main.y),
                ..main
            };
            self.draw_picker(frame, above);
        }
        frame.render_widget(
            Paragraph::new(self.status_line(status_area.width as usize)),
            status_area,
        );
    }

    fn draw_sidebar(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme.clone();
        frame.render_widget(Block::new().style(Style::new().bg(t.sidebar_bg)), area);
        if area.width < 6 || area.height < 4 {
            // Nothing is drawn, so nothing there should take clicks.
            self.sidebar_list = Rect::default();
            self.sidebar_footer = Rect::default();
            return;
        }
        // A one-column strip stands in for the GUI's 1px border.
        frame.render_widget(
            Block::new().style(Style::new().bg(t.sidebar_border)),
            Rect::new(area.right() - 1, area.y, 1, area.height),
        );
        let inner = Rect::new(area.x + 1, area.y, area.width - 3, area.height);
        let width = inner.width as usize;

        let (_, dot) = self.connection_state();
        let wordmark = row(
            vec![
                Span::styled(
                    "T3",
                    Style::new().fg(t.sidebar_fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" Code", Style::new().fg(t.sidebar_muted)),
            ],
            vec![Span::styled("●", Style::new().fg(dot))],
            width,
        );
        frame.render_widget(
            Paragraph::new(wordmark),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );

        // The shelf footer sits on the last row, as in the GUI.
        let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        self.sidebar_footer = footer;
        let chevron = if self.show_settled { "⌄" } else { "›" };
        let label = format!("Settled ({})", self.settled_count);
        let shelf = Line::from(vec![
            Span::styled(format!("{label} "), Style::new().fg(t.sidebar_muted)),
            Span::styled(
                "─".repeat(width.saturating_sub(label.width() + 3)),
                Style::new().fg(t.sidebar_border),
            ),
            Span::styled(format!(" {chevron}"), Style::new().fg(t.sidebar_muted)),
        ]);
        frame.render_widget(Paragraph::new(shelf), footer);

        let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 3);
        self.sidebar_list = list;
        let height = list.height as usize;
        // Keep the selected card in view.
        if self.selected < self.sidebar_offset {
            self.sidebar_offset = self.selected;
        }
        while self.sidebar_offset < self.selected {
            let used: usize = self.rows[self.sidebar_offset..=self.selected]
                .iter()
                .map(Row::height)
                .sum();
            if used <= height {
                break;
            }
            self.sidebar_offset += 1;
        }
        let open_id = self.open.as_ref().map(|o| o.id.clone());
        let mut lines: Vec<Line> = Vec::with_capacity(height);
        for (index, row) in self.rows.iter().enumerate().skip(self.sidebar_offset) {
            if lines.len() >= height {
                break;
            }
            match row {
                Row::Section(title) => {
                    lines.push(Line::default());
                    let rule = "─".repeat(width.saturating_sub(title.width() + 3));
                    lines.push(Line::from(vec![
                        Span::styled(format!("{title} "), Style::new().fg(t.sidebar_muted)),
                        Span::styled(rule, Style::new().fg(t.sidebar_border)),
                        Span::styled(" ⌄", Style::new().fg(t.sidebar_muted)),
                    ]));
                }
                Row::Thread(id) => {
                    let is_open = open_id.as_deref() == Some(id.as_str());
                    let cursor = index == self.selected;
                    let bg = if is_open {
                        Some(t.row_active)
                    } else if cursor {
                        Some(t.row_selected)
                    } else {
                        None
                    };
                    lines.extend(self.thread_card(id, width, bg, cursor, &t));
                    lines.push(Line::default());
                }
            }
        }
        lines.truncate(height);
        if lines.is_empty() {
            let hint = if self.shell.is_none() {
                "Loading threads…"
            } else {
                "No threads yet"
            };
            lines.push(Line::styled(hint, Style::new().fg(t.sidebar_muted)));
        }
        frame.render_widget(Paragraph::new(lines), list);
    }

    /// Three lines like the GUI's thread card: project and status, title, branch and provider.
    fn thread_card(
        &self,
        id: &str,
        width: usize,
        bg: Option<Color>,
        cursor: bool,
        t: &Theme,
    ) -> Vec<Line<'static>> {
        let thread = self.shell_thread(id);
        let title = thread
            .map(|t| str_of(t, "title"))
            .filter(|s| !s.is_empty())
            .unwrap_or("Untitled")
            .to_string();
        let project = thread
            .map(|t| self.project_title(str_of(t, "projectId")))
            .unwrap_or_default();
        let badge = monogram(&project);
        let (status_label, status_color) = self.thread_status(thread, t);
        let branch = thread
            .map(|t| str_of(t, "branch"))
            .unwrap_or("")
            .to_string();
        let (glyph, glyph_color) = t.provider_glyph(
            thread
                .map(|t| str_of(&t["modelSelection"], "instanceId"))
                .unwrap_or(""),
        );
        let bar = if cursor {
            let color = if self.focus == Focus::Sidebar {
                t.primary
            } else {
                t.sidebar_muted
            };
            Span::styled("▎", Style::new().fg(color))
        } else {
            Span::raw(" ")
        };
        let inner = width.saturating_sub(2);
        let muted = Style::new().fg(t.sidebar_muted);
        let project_room = inner.saturating_sub(badge.width() + status_label.width() + 2);
        let line1 = row(
            vec![
                Span::styled(
                    badge.clone(),
                    Style::new()
                        .fg(t.project_color(&project))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {}", fit(&project, project_room)), muted),
            ],
            vec![Span::styled(status_label, Style::new().fg(status_color))],
            inner,
        );
        let line2 = row(
            vec![Span::styled(
                fit(&title, inner),
                Style::new().fg(t.sidebar_fg),
            )],
            Vec::new(),
            inner,
        );
        let line3 = row(
            vec![Span::styled(fit(&branch, inner.saturating_sub(2)), muted)],
            vec![Span::styled(glyph, Style::new().fg(glyph_color))],
            inner,
        );
        [line1, line2, line3]
            .into_iter()
            .map(|line| {
                let mut spans = vec![bar.clone()];
                spans.extend(line.spans);
                spans.push(Span::raw(" "));
                match bg {
                    Some(bg) => Line::from(with_bg(spans, bg)),
                    None => Line::from(spans),
                }
            })
            .collect()
    }

    /// The card's right-hand label: a pending request, the working clock, a failure, or the
    /// time of the latest activity.
    fn thread_status(&self, thread: Option<&Value>, t: &Theme) -> (String, Color) {
        let Some(thread) = thread else {
            return (String::new(), t.sidebar_muted);
        };
        let request = &thread["pendingRuntimeRequest"];
        if !request.is_null() {
            return if str_of(request, "kind") == "user_input" {
                ("Input".into(), t.indigo)
            } else {
                ("Approval".into(), t.warning_fg)
            };
        }
        let state = status(thread);
        if is_active_status(state) {
            let since = parse_iso_ms(str_of(thread, "latestRunStartedAt"))
                .map(|started| self.now - started)
                .unwrap_or(0);
            return (
                format!("{} {}", self.spinner(), theme::working_label(since)),
                t.info,
            );
        }
        match state {
            "failed" => return ("Failed".into(), t.error_fg),
            "queued" => return ("Queued".into(), t.sidebar_muted),
            _ => {}
        }
        let stamp = [
            str_of(thread, "latestUserMessageAt"),
            str_of(thread, "updatedAt"),
        ]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_default();
        (relative_time(stamp, self.now), t.sidebar_muted)
    }

    /// The GUI's breadcrumb: project, a slash, the thread title.
    fn header_line(&self, width: usize) -> Line<'static> {
        let t = &self.theme;
        let muted = Style::new().fg(t.muted);
        let Some(open) = self.open.as_ref() else {
            return Line::styled("Pick a thread in the sidebar", muted);
        };
        let Some(thread) = self.open_thread_json() else {
            return Line::styled("Loading…", muted);
        };
        let project = self.project_title(str_of(thread, "projectId"));
        let title = str_of(thread, "title").to_string();
        let badge = monogram(&project);
        let right = if open.connection == "live" {
            Vec::new()
        } else {
            vec![Span::styled(
                open.connection.clone(),
                Style::new().fg(t.warning_fg),
            )]
        };
        let right_width: usize = right.iter().map(|s| s.content.width()).sum();
        let room = width.saturating_sub(badge.width() + project.width() + 7 + right_width);
        row(
            vec![
                Span::styled(
                    badge,
                    Style::new()
                        .fg(t.project_color(&project))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {project}"), muted),
                Span::styled("  /  ", Style::new().fg(t.border_strong)),
                Span::styled(fit(&title, room), Style::new().fg(t.fg)),
            ],
            right,
            width,
        )
    }

    fn draw_transcript(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme.clone();
        let muted = Style::new().fg(t.muted);
        let Some((state, run_changes)) = self
            .open
            .as_ref()
            .and_then(|o| Some((o.state.as_ref()?, &o.run_changes)))
        else {
            let hint = if self.open.is_some() {
                "Loading thread…"
            } else {
                "Select a thread with ↑/↓ and press Enter."
            };
            frame.render_widget(Paragraph::new(Line::styled(hint, muted)), area);
            return;
        };
        let width = area.width.saturating_sub(1).max(10);
        let blocks = transcript::blocks(state);
        let active_run = state.active_run().map(|r| str_of(r, "id").to_string());
        let expand = self.expand_tools;
        let context = RenderContext {
            theme: &t,
            text: Styles::new(&t, t.text()),
            bubble: Styles::new(&t, Style::new().fg(t.fg).bg(t.bubble)),
            expand,
        };
        // Each block keeps its wrapped lines until its content or the width changes. Only a
        // streaming block, or the clock row under a running prompt, is rewrapped per frame.
        let mut heights = Vec::with_capacity(blocks.len());
        for block in &blocks {
            let activity = matches!(
                block.kind,
                BlockKind::Tool | BlockKind::Reasoning | BlockKind::Request
            );
            // Finished runs fold their activity away, as the GUI does, until `t` expands it.
            if activity && !expand && active_run.as_deref() != Some(block.run_id.as_str()) {
                heights.push(0);
                continue;
            }
            // A prompt's clock row comes from its run. The running prompt's ticks each second;
            // a finished one changes only with its run, so it is rebuilt only then.
            let (clock, stamp) = match block.kind {
                BlockKind::User if active_run.as_deref() == Some(block.run_id.as_str()) => {
                    ("live", (self.now / 1000) as u64)
                }
                BlockKind::User => ("done", run_changes.get(&block.run_id).copied().unwrap_or(0)),
                _ => ("", 0),
            };
            let decision = match block.kind {
                BlockKind::Request => decision_for(state, &block.request_id),
                _ => String::new(),
            };
            let key = (
                hash(
                    &[
                        &block.header,
                        &block.body,
                        &block.status,
                        if expand { "1" } else { "0" },
                        clock,
                        &decision,
                    ],
                    stamp,
                ),
                width,
            );
            let fresh = self.cache.get(&block.item_id).is_none_or(|c| c.key != key);
            if fresh {
                let extra = match block.kind {
                    BlockKind::User => self
                        .fold_for(state, &block.run_id)
                        .map_or(Extra::None, Extra::Fold),
                    BlockKind::Request => Extra::Decision(decision),
                    _ => Extra::None,
                };
                let lines = render_block(block, width as usize, &context, &extra);
                self.cache
                    .insert(block.item_id.clone(), Cached { key, lines });
            }
            heights.push(self.cache[&block.item_id].lines.len());
        }
        let total: usize = heights.iter().sum();
        let height = area.height as usize;
        self.scroll = self.scroll.min(total.saturating_sub(height));
        let end = total - self.scroll;
        let start = end.saturating_sub(height);

        // Copy only the rows in view.
        let mut lines = Vec::with_capacity(height);
        let mut offset = 0;
        for (block, block_height) in blocks.iter().zip(&heights) {
            if *block_height > 0 && offset + block_height > start && offset < end {
                let cached = &self.cache[&block.item_id].lines;
                let from = start.saturating_sub(offset);
                let to = (end - offset).min(*block_height);
                lines.extend(cached[from..to].iter().cloned());
            }
            offset += block_height;
            if offset >= end {
                break;
            }
        }
        frame.render_widget(Paragraph::new(lines), area);
        if self.scroll > 0 {
            let label = format!(" ↓ {} more ", self.scroll);
            let x = area.x + area.width.saturating_sub(label.width() as u16 + 1);
            frame.render_widget(
                Paragraph::new(Line::styled(
                    label.clone(),
                    Style::new().fg(t.fg).bg(t.primary),
                )),
                Rect::new(x, area.y + area.height - 1, label.width() as u16, 1),
            );
        }
    }

    /// The clock row under a prompt: `Working for 12s` while the run is active, then
    /// `Worked for 2m 32s`, as in the GUI.
    fn fold_for(&self, state: &ThreadState, run_id: &str) -> Option<Fold> {
        let run = state.run(run_id)?;
        let started = parse_iso_ms(str_of(run, "startedAt"))?;
        let state_now = status(run);
        let active = is_active_status(state_now);
        let ended = if active {
            self.now
        } else {
            parse_iso_ms(str_of(run, "completedAt")).unwrap_or(self.now)
        };
        let elapsed = duration_label(ended - started);
        let (label, tone) = if active {
            (
                format!("{} Working for {elapsed}", self.spinner()),
                Tone::Info,
            )
        } else {
            match state_now {
                "failed" => (format!("Failed after {elapsed}"), Tone::Error),
                "interrupted" | "cancelled" => (format!("Stopped after {elapsed}"), Tone::Muted),
                _ => (format!("Worked for {elapsed}"), Tone::Muted),
            }
        };
        Some(Fold {
            label,
            tone,
            expanded: active || self.expand_tools,
        })
    }

    /// The command an approval request gates. It must be the same request that Alt+A answers,
    /// so it comes from the request's own node, never from what else is running.
    fn pending_command(&self, request_id: &str) -> Option<String> {
        let state = self.open.as_ref()?.state.as_ref()?;
        let item = state.request_subject(request_id)?;
        if str_of(item, "type") != "command_execution" {
            return None;
        }
        Some(str_of(item, "input").to_string()).filter(|input| !input.trim().is_empty())
    }

    fn request_panel(&self, width: usize) -> Option<RequestPanel> {
        let (request, item) = self.pending_request()?;
        let t = &self.theme;
        let muted = Style::new().fg(t.muted);
        let text = Style::new().fg(t.fg);
        let item = item.unwrap_or(Value::Null);
        if request["kind"] == "user_input" {
            let answered = self.open.as_ref().map_or(0, |o| o.answers.len());
            let questions = item["questions"].as_array().cloned().unwrap_or_default();
            let question = questions.get(answered)?;
            let mut choices = Vec::new();
            for (index, option) in question["options"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .take(6)
            {
                let description = str_of(option, "description");
                let mut spans = vec![
                    Span::styled(format!("  {}. ", index + 1), muted),
                    Span::styled(str_of(option, "label").to_string(), text),
                ];
                if !description.is_empty() {
                    spans.push(Span::styled(format!("  {description}"), muted));
                }
                choices.push(Line::from(spans));
            }
            choices.push(Line::styled(
                "  Type a number or your own answer, then Enter.",
                muted,
            ));
            return Some(RequestPanel {
                key: format!("{}#{answered}", str_of(&request, "id")),
                title: vec![Span::styled(
                    format!("◈ Question {} of {}", answered + 1, questions.len()),
                    Style::new().fg(t.indigo).add_modifier(Modifier::BOLD),
                )],
                text: markdown::render(
                    str_of(question, "question"),
                    width,
                    &Styles::new(t, text),
                    2,
                ),
                choices,
            });
        }
        let kind = str_of(&request, "kind");
        let title = match kind {
            "command" => "Command approval".to_string(),
            "file" | "edit" | "file_change" => "Edit approval".to_string(),
            other => format!("{} approval", capitalize(other)),
        };
        let mut lines = Vec::new();
        let prompt = str_of(&item, "prompt");
        if !prompt.is_empty() {
            lines.extend(markdown::render(prompt, width, &Styles::new(t, muted), 2));
        }
        // The command waiting on this approval, as the GUI shows it under the label. All of
        // it, wrapped rather than cut, since this is what Alt+A runs.
        if kind == "command"
            && let Some(command) = self.pending_command(str_of(&request, "id"))
        {
            for line in command.lines() {
                for part in wrap_chars(line, width.saturating_sub(2)) {
                    lines.push(Line::styled(format!("  {part}"), text));
                }
            }
        }
        let button = |label: &str, primary: bool| {
            let style = if primary {
                Style::new()
                    .fg(t.fg)
                    .bg(t.primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(t.fg).bg(t.chip_bg)
            };
            Span::styled(format!(" {label} "), style)
        };
        let choices = vec![Line::from(vec![
            Span::raw("  "),
            button("Approve", true),
            Span::styled(" Alt+A   ", muted),
            button("Always allow", false),
            Span::styled(" Alt+S   ", muted),
            button("Decline", false),
            Span::styled(" Alt+D", muted),
        ])];
        Some(RequestPanel {
            key: str_of(&request, "id").to_string(),
            title: vec![Span::styled(
                format!("◈ {title}"),
                Style::new().fg(t.warning_fg).add_modifier(Modifier::BOLD),
            )],
            text: lines,
            choices,
        })
    }

    /// The panel's rows within `rows` lines of room, scrolled to where the user left it.
    fn panel_rows(&mut self, panel: RequestPanel, width: usize, rows: usize) -> Vec<Line<'static>> {
        if panel.key != self.panel_key {
            self.panel_key = panel.key.clone();
            self.panel_scroll = 0;
        }
        let muted = Style::new().fg(self.theme.muted);
        lay_out_panel(panel, width, rows, &mut self.panel_scroll, muted)
    }

    fn draw_request_panel(&self, frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
        let t = &self.theme;
        let question = self
            .pending_request()
            .is_some_and(|(r, _)| r["kind"] == "user_input");
        let (border, bg) = if question {
            (t.indigo_border, t.indigo_surface)
        } else {
            (t.warning_border, t.warning_surface)
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border))
            .style(Style::new().bg(bg));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let inner = Rect {
            x: inner.x + 1,
            width: inner.width.saturating_sub(2),
            ..inner
        };
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn placeholder(&self) -> &'static str {
        if self.open.is_none() {
            return "Pick a thread in the sidebar to start";
        }
        match self.pending_request() {
            Some((request, _)) if request["kind"] == "user_input" => {
                "Type a number or your own answer, then Enter"
            }
            Some(_) => "Resolve this approval request to continue",
            None if self.connection_state().0 != "live" => "Connecting to the thread…",
            None => "Ask anything, or send a follow-up to this thread",
        }
    }

    fn draw_composer(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        rows: &[String],
        cursor: (usize, usize),
    ) {
        self.chips.clear();
        let t = &self.theme;
        let focused = self.focus == Focus::Composer;
        let border = if focused { t.border_strong } else { t.border };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border))
            .style(Style::new().bg(t.raised));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height < 2 || inner.width < 4 {
            return;
        }
        let text_area = Rect::new(
            inner.x + 1,
            inner.y,
            inner.width - 2,
            inner.height.saturating_sub(2).max(1),
        );
        let chips_area = Rect::new(inner.x + 1, inner.bottom() - 1, inner.width - 2, 1);
        let visible = text_area.height as usize;
        let first = (cursor.0 + 1).saturating_sub(visible);
        let lines: Vec<Line> = if self.composer.text().is_empty() {
            vec![Line::styled(
                fit(self.placeholder(), text_area.width as usize),
                Style::new().fg(t.muted),
            )]
        } else {
            rows.iter()
                .skip(first)
                .take(visible)
                .map(|r| Line::styled(r.clone(), Style::new().fg(t.fg)))
                .collect()
        };
        frame.render_widget(Paragraph::new(lines), text_area);
        if focused {
            frame.set_cursor_position((
                text_area.x + cursor.1 as u16,
                text_area.y + (cursor.0 - first) as u16,
            ));
        }
        let (chips_line, chips) = self.composer_chips(chips_area.width as usize);
        frame.render_widget(Paragraph::new(chips_line), chips_area);
        self.chips = chips
            .into_iter()
            .filter(|(start, _, _)| *start < chips_area.width as usize)
            .map(|(start, width, kind)| {
                let x = chips_area.x + start as u16;
                let width = (width as u16).min(chips_area.right() - x);
                (Rect::new(x, chips_area.y, width, 1), kind)
            })
            .collect();
    }

    /// The composer footer: model, effort and other options, runtime mode, plan mode and the
    /// send button. Returns each chip's column, width and menu. A choice waiting for the next
    /// message shows in the info color.
    fn composer_chips(&self, width: usize) -> (Line<'static>, Vec<(usize, usize, Kind)>) {
        let t = &self.theme;
        let muted = Style::new().fg(t.muted);
        let pending = Style::new().fg(t.info_fg);
        let divider = Span::styled("  │  ", Style::new().fg(t.border_strong));
        let mut left: Vec<Span<'static>> = Vec::new();
        let mut chips = Vec::new();
        if let (Some(thread), Some(view)) = (self.open_thread_json(), self.settings_view()) {
            let config = self.config.as_ref();
            let (current, selection) = (&thread["modelSelection"], &view.selection);
            let (glyph, color) = t.provider_glyph(str_of(selection, "instanceId"));
            let model = config
                .and_then(|config| picker::selected_model(config, selection))
                .and_then(|(_, model)| model["name"].as_str().map(str::to_string))
                .unwrap_or_else(|| model_display_name(str_of(selection, "model")));
            let new_model = selection["instanceId"] != current["instanceId"]
                || selection["model"] != current["model"];
            push_chip(
                &mut left,
                &mut chips,
                Kind::Model,
                vec![
                    Span::styled(format!("{glyph} "), Style::new().fg(color)),
                    Span::styled(model, if new_model { pending } else { muted }),
                    Span::styled(" ▾", muted),
                ],
            );
            if let Some(traits) = config.and_then(|config| picker::traits_label(config, &view)) {
                let new_traits =
                    view.prompt_effort.is_some() || selection["options"] != current["options"];
                left.push(divider.clone());
                push_chip(
                    &mut left,
                    &mut chips,
                    Kind::Traits,
                    vec![
                        Span::styled(traits, if new_traits { pending } else { muted }),
                        Span::styled(" ▾", muted),
                    ],
                );
            }
            left.push(divider.clone());
            let mode = runtime_mode_label(&view.runtime_mode);
            let new_mode = view.plan.runtime_mode.is_some();
            push_chip(
                &mut left,
                &mut chips,
                Kind::Mode,
                vec![Span::styled(
                    format!("⊡ {mode}"),
                    if new_mode { pending } else { muted },
                )],
            );
            if view.interaction_mode == "plan" {
                let style = if view.plan.interaction_mode.is_some() {
                    pending
                } else {
                    Style::new().fg(t.violet)
                };
                left.push(divider);
                push_chip(
                    &mut left,
                    &mut chips,
                    Kind::Mode,
                    vec![Span::styled("Plan", style)],
                );
            }
        }
        let send = if self.composer.is_empty() {
            Style::new().fg(t.muted).bg(t.chip_bg)
        } else {
            Style::new()
                .fg(t.fg)
                .bg(t.primary)
                .add_modifier(Modifier::BOLD)
        };
        let right = vec![Span::styled("Enter ", muted), Span::styled(" ↑ ", send)];
        (row(left, right, width), chips)
    }

    /// The open menu, drawn in `above` just over the composer and lined up with its chip.
    fn draw_picker(&mut self, frame: &mut Frame, above: Rect) {
        let Some(kind) = self.picker.as_ref().map(|p| p.kind) else {
            return;
        };
        let t = self.theme.clone();
        let muted = Style::new().fg(t.muted);
        let items = self.picker_items();
        let empty = match (&self.config, kind) {
            (None, _) => "Loading T3's model list…",
            (_, Kind::Model) => "No model matches",
            (_, Kind::Traits) => "This model has no options",
            (_, Kind::Mode) => "",
        };
        let searching = kind == Kind::Model;
        // Section labels, then entries as a check column and the label.
        let content = items
            .iter()
            .map(|item| match item {
                Item::Heading { text, provider } => text.width() + 2 * provider.is_some() as usize,
                Item::Entry { label, .. } => 3 + label.width(),
            })
            .max()
            .unwrap_or(0)
            .max(empty.width())
            .max(if searching { 30 } else { 16 });
        let width = (content as u16 + 4).min(above.width);
        let list_height = items.len().max(1) as u16;
        let search_height = if searching { 2 } else { 0 };
        let height = (list_height + search_height + 2).min(above.height);
        if height < 3 + search_height || width < 8 {
            return;
        }
        let chip_x = self
            .chips
            .iter()
            .find(|(_, chip)| *chip == kind)
            .map_or(above.x, |(area, _)| area.x);
        let x = chip_x
            .saturating_sub(2)
            .clamp(above.x, above.right().saturating_sub(width));
        let area = Rect::new(x, above.bottom() - height, width, height);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.border_strong))
            .style(Style::new().bg(t.popover));
        let inner = block.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        self.picker_area = area;

        let mut list = inner;
        if searching {
            let filter = self
                .picker
                .as_ref()
                .map(|p| p.filter.clone())
                .unwrap_or_default();
            let line = if filter.is_empty() {
                Line::from(vec![
                    Span::styled(" ⌕ ", muted),
                    Span::styled("Search models", muted),
                ])
            } else {
                Line::from(vec![
                    Span::styled(" ⌕ ", muted),
                    Span::styled(
                        fit(&filter, inner.width as usize - 4),
                        Style::new().fg(t.fg),
                    ),
                ])
            };
            frame.render_widget(Paragraph::new(line), Rect { height: 1, ..inner });
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "─".repeat(inner.width as usize),
                    Style::new().fg(t.border),
                )),
                Rect {
                    y: inner.y + 1,
                    height: 1,
                    ..inner
                },
            );
            let cursor = 3 + filter.width().min(inner.width as usize - 4);
            frame.set_cursor_position((inner.x + cursor as u16, inner.y));
            list = Rect {
                y: inner.y + 2,
                height: inner.height - 2,
                ..inner
            };
        }
        if items.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(format!(" {empty}"), muted)),
                list,
            );
            return;
        }

        // Keep the highlight in view, with its section label when it is the section's first.
        let visible = list.height as usize;
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        // A refreshed model list can be shorter than the one the highlight was on.
        picker.selected = picker.selected.min(items.len() - 1);
        let selected = picker.selected;
        if selected < picker.offset {
            picker.offset = selected;
        }
        if selected >= picker.offset + visible {
            picker.offset = selected + 1 - visible;
        }
        if picker.offset > 0
            && picker.offset == selected
            && !items[selected - 1].is_entry()
            && visible > 1
        {
            picker.offset -= 1;
        }
        let offset = picker.offset;

        let row_width = list.width as usize;
        let lines: Vec<Line> = items
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
            .map(|(index, item)| {
                self.picker_rows
                    .push((list.y + (index - offset) as u16, index));
                match item {
                    Item::Heading { text, provider } => {
                        let mut spans = vec![Span::raw(" ")];
                        if let Some(provider) = provider {
                            let (glyph, color) = t.provider_glyph(provider);
                            spans.push(Span::styled(format!("{glyph} "), Style::new().fg(color)));
                        }
                        spans.push(Span::styled(text.clone(), muted));
                        Line::from(spans)
                    }
                    Item::Entry { label, current, .. } => {
                        let check = if *current { " ✓ " } else { "   " };
                        let left = vec![
                            Span::styled(check, Style::new().fg(t.primary)),
                            Span::styled(
                                fit(label, row_width.saturating_sub(4)),
                                Style::new().fg(t.fg),
                            ),
                        ];
                        let line = row(left, Vec::new(), row_width);
                        if index == selected {
                            Line::from(with_bg(line.spans, t.highlight))
                        } else {
                            line
                        }
                    }
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), list);
    }

    fn status_line(&self, width: usize) -> Line<'static> {
        let t = &self.theme;
        if let Some((text, error)) = &self.message {
            let color = if *error { t.error_fg } else { t.success };
            return Line::styled(fit(text, width), Style::new().fg(color));
        }
        let (connection, color) = self.connection_state();
        let unsent = self
            .open
            .as_ref()
            .is_some_and(|o| self.unsent.contains_key(&o.id));
        let keys = match (self.focus, self.picker.as_ref().map(|p| p.kind)) {
            (_, Some(Kind::Model)) => "Type to search · ↑↓ choose · Enter select · Esc close",
            (_, Some(_)) => "↑↓ choose · Enter select · Esc close",
            (Focus::Sidebar, None) => "↑↓ select · Enter open · e settled · Tab focus · q quit",
            (Focus::Composer, None) => {
                "Enter send · Alt+Enter newline · Alt+M model · Alt+E effort · Alt+P mode · Ctrl+X interrupt"
            }
            (Focus::Transcript, None) => {
                "↑↓/PgUp scroll · G bottom · t activity · Enter compose · Esc sidebar"
            }
        };
        let head = fit(&format!("{connection}  ·  "), width.saturating_sub(2));
        let mut room = width.saturating_sub(2 + head.width());
        let mut spans = vec![
            Span::styled("● ", Style::new().fg(color)),
            Span::styled(head, Style::new().fg(t.muted)),
        ];
        if unsent && self.picker.is_none() && room > 0 {
            let note = fit("Unsent message: Ctrl+R  ·  ", room);
            room -= note.width();
            spans.push(Span::styled(note, Style::new().fg(t.warning_fg)));
        }
        if room > 0 {
            spans.push(Span::styled(fit(keys, room), Style::new().fg(t.muted)));
        }
        Line::from(spans)
    }
}

// ---- block rendering ----

struct RenderContext<'a> {
    theme: &'a Theme,
    text: Styles,
    bubble: Styles,
    expand: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Info,
    Error,
    Muted,
}

struct Fold {
    label: String,
    tone: Tone,
    expanded: bool,
}

/// A pending request's panel, in the parts that lay out differently when space is short.
struct RequestPanel {
    /// Changes when the panel shows a different request or question, which resets its scroll.
    key: String,
    title: Vec<Span<'static>>,
    text: Vec<Line<'static>>,
    choices: Vec<Line<'static>>,
}

/// Per-block state that lives outside the block. What it is made from joins the cache key.
enum Extra {
    None,
    Fold(Fold),
    Decision(String),
}

/// What became of a runtime request, for the row that shows it in the transcript.
fn decision_for(state: &ThreadState, request_id: &str) -> String {
    let Some(request) = state
        .list("runtimeRequests")
        .iter()
        .find(|r| str_of(r, "id") == request_id)
    else {
        return String::new();
    };
    match (str_of(request, "status"), str_of(request, "decision")) {
        ("pending", _) => "Pending",
        (_, "accept") | (_, "acceptForSession") => "Approved",
        (_, "decline") => "Declined",
        ("resolved", _) => "Resolved",
        _ => "",
    }
    .to_string()
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Gives spans without a background the card's background.
fn with_bg(spans: Vec<Span<'static>>, bg: Color) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|mut span| {
            if span.style.bg.is_none() {
                span.style = span.style.bg(bg);
            }
            span
        })
        .collect()
}

/// Appends a chip's spans to `left` and records its column, width and menu.
fn push_chip(
    left: &mut Vec<Span<'static>>,
    chips: &mut Vec<(usize, usize, Kind)>,
    kind: Kind,
    spans: Vec<Span<'static>>,
) {
    let start: usize = left.iter().map(|s| s.content.width()).sum();
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    left.extend(spans);
    chips.push((start, width, kind));
}

/// `left` at the start, `right` at the end, padded to exactly `width` columns.
fn row(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let left_width: usize = left.iter().map(|s| s.content.width()).sum();
    let right_width: usize = right.iter().map(|s| s.content.width()).sum();
    let mut spans = left;
    if right_width > 0 && left_width + right_width <= width {
        spans.push(Span::raw(" ".repeat(width - left_width - right_width)));
        spans.extend(right);
    } else {
        spans.push(Span::raw(" ".repeat(width.saturating_sub(left_width))));
    }
    Line::from(spans)
}

/// A request panel within `rows` lines of room. The title and the choices always show. The
/// text between them scrolls with Alt+↑/↓ or the wheel when it doesn't fit, and `scroll` is
/// clamped to what can scroll.
fn lay_out_panel(
    panel: RequestPanel,
    width: usize,
    rows: usize,
    scroll: &mut usize,
    muted: Style,
) -> Vec<Line<'static>> {
    let text_rows = rows.saturating_sub(1 + panel.choices.len());
    let total = panel.text.len();
    let mut lines = Vec::with_capacity(rows);
    if total <= text_rows {
        *scroll = 0;
        lines.push(Line::from(panel.title));
        lines.extend(panel.text);
    } else {
        *scroll = (*scroll).min(total - text_rows);
        let hint = if text_rows == 0 {
            "Make the window taller to read this".to_string()
        } else {
            format!(
                "Lines {}-{} of {total} · Alt+↑/↓ scroll",
                *scroll + 1,
                *scroll + text_rows
            )
        };
        lines.push(row(panel.title, vec![Span::styled(hint, muted)], width));
        lines.extend(panel.text.into_iter().skip(*scroll).take(text_rows));
    }
    lines.extend(panel.choices);
    lines
}

/// Breaks `text` into rows of at most `width` columns, by character.
fn wrap_chars(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("one row").push(c);
        used += w;
    }
    rows
}

fn fit(text: &str, width: usize) -> String {
    let text = text.replace('\n', " ");
    if text.width() <= width {
        return text;
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.width() + 2 > width {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// The icon and label for a tool row, after the GUI's Eye/SquarePen/Terminal/Globe/Search set.
fn tool_row(block: &transcript::Block) -> (&'static str, String) {
    match block.item_type.as_str() {
        "command_execution" => ("❯", block.detail.clone()),
        "file_change" => (
            "✎",
            format!("Edit {}", block.header.trim_start_matches("edit ")),
        ),
        "file_search" => ("⌕", format!("Search {}", block.detail)),
        "web_search" => ("◎", format!("Searched {}", block.detail)),
        "subagent" => ("⧉", block.header.clone()),
        "dynamic_tool" => {
            let name = block.tool_name.to_ascii_lowercase();
            let icon = if ["read", "view", "cat", "open"]
                .iter()
                .any(|n| name.contains(n))
            {
                "◉"
            } else if ["edit", "write", "patch", "create"]
                .iter()
                .any(|n| name.contains(n))
            {
                "✎"
            } else if ["bash", "shell", "exec", "command", "terminal"]
                .iter()
                .any(|n| name.contains(n))
            {
                "❯"
            } else if ["grep", "glob", "search", "find", "ls"]
                .iter()
                .any(|n| name.contains(n))
            {
                "⌕"
            } else if ["web", "fetch", "http", "browser"]
                .iter()
                .any(|n| name.contains(n))
            {
                "◎"
            } else if ["agent", "task", "delegate"]
                .iter()
                .any(|n| name.contains(n))
            {
                "⧉"
            } else {
                "⚙"
            };
            (icon, block.header.clone())
        }
        _ => ("⚙", block.header.clone()),
    }
}

/// The user's prompt as a right-aligned bubble no wider than 80% of the pane.
fn bubble_lines(text: &str, width: usize, context: &RenderContext) -> Vec<Line<'static>> {
    let bubble = Style::new().bg(context.theme.bubble);
    let max_text = (width * 4 / 5).saturating_sub(4).max(8);
    let inner = markdown::render(text.trim_end(), max_text, &context.bubble, 0);
    let text_width = inner
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .min(max_text);
    let left = width.saturating_sub(text_width + 4);
    // One padded row above and below, like the GUI's bubble padding.
    let padding = Line::from(vec![
        Span::raw(" ".repeat(left)),
        Span::styled(" ".repeat(text_width + 4), bubble),
    ]);
    let mut lines = vec![padding.clone()];
    lines.extend(inner.into_iter().map(|line| {
        let used = line.width();
        let mut spans = vec![Span::raw(" ".repeat(left)), Span::styled("  ", bubble)];
        spans.extend(with_bg(line.spans, context.theme.bubble));
        spans.push(Span::styled(
            " ".repeat(text_width.saturating_sub(used) + 2),
            bubble,
        ));
        Line::from(spans)
    }));
    lines.push(padding);
    lines
}

fn render_block(
    block: &transcript::Block,
    width: usize,
    context: &RenderContext,
    extra: &Extra,
) -> Vec<Line<'static>> {
    let t = context.theme;
    let muted = Style::new().fg(t.muted);
    let hairline = Style::new().fg(t.border);
    let mut lines = Vec::new();
    match block.kind {
        BlockKind::User => {
            lines.extend(bubble_lines(&block.body, width, context));
            lines.push(Line::default());
            if let Extra::Fold(fold) = extra {
                let color = match fold.tone {
                    Tone::Info => t.info,
                    Tone::Error => t.error_fg,
                    Tone::Muted => t.muted,
                };
                let chevron = if fold.expanded { "⌄" } else { "›" };
                lines.push(Line::styled(
                    format!("{} {chevron}", fold.label),
                    Style::new().fg(color),
                ));
                lines.push(Line::styled("─".repeat(width), hairline));
            }
        }
        BlockKind::Assistant => {
            lines.push(Line::default());
            lines.extend(markdown::render(&block.body, width, &context.text, 0));
            lines.push(Line::default());
        }
        BlockKind::Reasoning => {
            // One line, like the GUI's collapsed reasoning row. `threads read --reasoning`
            // prints the whole text.
            let label = if block.streaming {
                "Thinking"
            } else {
                "Thought"
            };
            let first = block
                .body
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default()
                .trim_matches(|c: char| c == '*' || c == '#' || c.is_whitespace());
            lines.push(Line::from(vec![
                Span::styled(format!("✦ {label}"), muted),
                Span::styled(
                    fit(
                        &format!(" · {first}"),
                        width.saturating_sub(label.width() + 2),
                    ),
                    muted.add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        BlockKind::Tool => {
            let (icon, label) = tool_row(block);
            let color = match block.status.as_str() {
                "running" | "pending" | "inProgress" | "in_progress" => t.info,
                "failed" | "error" => t.error_fg,
                _ => t.muted,
            };
            let exit = block.exit_code.filter(|code| *code != 0);
            let room = width.saturating_sub(2 + exit.map_or(0, |_| 10));
            let mut spans = vec![Span::styled(
                format!("{icon} {}", fit(&label, room)),
                Style::new().fg(color),
            )];
            if let Some(code) = exit {
                spans.push(Span::styled(
                    format!("  exit {code}"),
                    Style::new().fg(t.error_fg),
                ));
            }
            lines.push(Line::from(spans));
            if context.expand && !block.body.is_empty() {
                for line in block.body.lines() {
                    lines.push(Line::from(vec![
                        Span::styled("  │ ", Style::new().fg(t.border_strong)),
                        Span::styled(fit(line, width.saturating_sub(4)), muted),
                    ]));
                }
            }
        }
        BlockKind::Request => {
            let decision = match extra {
                Extra::Decision(decision) => decision.as_str(),
                _ => "",
            };
            let color = match decision {
                "Pending" => t.warning_fg,
                "Approved" => t.success,
                "Declined" => t.error_fg,
                _ => t.muted,
            };
            let title = if block.item_type == "user_input_request" {
                "Question".to_string()
            } else {
                let kind = block.header.rsplit(": ").next().unwrap_or("command");
                if kind == "command" {
                    "Command approval".to_string()
                } else {
                    format!("{} approval", capitalize(kind))
                }
            };
            let room = width.saturating_sub(title.width() + decision.width() + 6);
            let mut left = vec![
                Span::styled("◈ ", Style::new().fg(color)),
                Span::styled(title, muted),
            ];
            if block.item_type != "user_input_request" && !block.detail.is_empty() {
                left.push(Span::styled(" · ", Style::new().fg(t.border_strong)));
                left.push(Span::styled(
                    fit(&block.detail, room),
                    Style::new().fg(t.fg),
                ));
            }
            lines.push(row(
                left,
                vec![Span::styled(decision.to_string(), Style::new().fg(color))],
                width,
            ));
            if block.item_type == "user_input_request" {
                for question in block.body.lines() {
                    lines.push(Line::styled(fit(&format!("  {question}"), width), muted));
                }
            }
        }
        BlockKind::Plan => {
            lines.push(Line::default());
            lines.push(Line::styled(
                block.header.clone(),
                Style::new().fg(t.violet).add_modifier(Modifier::BOLD),
            ));
            lines.extend(markdown::render(&block.body, width, &context.text, 2));
            lines.push(Line::default());
        }
        BlockKind::Notice => {
            lines.push(Line::default());
            let title = fit(&block.header, width.saturating_sub(6));
            let side = width.saturating_sub(title.width() + 2) / 2;
            let rest = width.saturating_sub(side + title.width() + 2);
            lines.push(Line::from(vec![
                Span::styled("─".repeat(side), hairline),
                Span::styled(format!(" {title} "), muted),
                Span::styled("─".repeat(rest), hairline),
            ]));
            lines.push(Line::default());
        }
        BlockKind::Error => {
            lines.push(Line::default());
            lines.push(Line::styled(
                "✕ Error",
                Style::new().fg(t.error_fg).add_modifier(Modifier::BOLD),
            ));
            lines.extend(markdown::render(
                &block.body,
                width,
                &Styles::new(t, Style::new().fg(t.error_fg)),
                2,
            ));
            lines.push(Line::default());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use theme::Depth;

    fn block(kind: BlockKind, item_type: &str, header: &str, body: &str) -> transcript::Block {
        transcript::Block {
            item_id: "item".into(),
            kind,
            header: header.into(),
            body: body.into(),
            streaming: false,
            status: "completed".into(),
            item_type: item_type.into(),
            detail: header.into(),
            exit_code: None,
            run_id: "run".into(),
            tool_name: String::new(),
            request_id: String::new(),
        }
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn user_prompts_render_as_right_aligned_bubbles_with_a_clock_row() {
        let theme = Theme::new(Depth::TrueColor);
        let context = RenderContext {
            theme: &theme,
            text: Styles::new(&theme, theme.text()),
            bubble: Styles::new(&theme, Style::new().fg(theme.fg).bg(theme.bubble)),
            expand: false,
        };
        let fold = Extra::Fold(Fold {
            label: "Worked for 2m 32s".into(),
            tone: Tone::Muted,
            expanded: false,
        });
        let lines = render_block(
            &block(BlockKind::User, "user_message", "You", "List the functions"),
            40,
            &context,
            &fold,
        );
        let rows = text(&lines);
        // Padding on both sides, pushed to the right edge of the pane.
        assert_eq!(rows[1], format!("{}  List the functions  ", " ".repeat(18)));
        assert_eq!(rows[1].width(), 40);
        assert_eq!(rows[0], rows[2], "padding rows above and below");
        assert!(lines[1].spans[1].style.bg == Some(theme.bubble));
        assert!(lines[1].spans[0].style.bg.is_none());
        assert_eq!(rows[4], "Worked for 2m 32s ›");
        assert_eq!(rows[5], "─".repeat(40));

        // Long prompts wrap inside 80% of the width.
        let long = render_block(
            &block(BlockKind::User, "user_message", "You", &"word ".repeat(30)),
            40,
            &context,
            &Extra::None,
        );
        assert!(text(&long).iter().all(|r| r.width() <= 40));
        assert!(long.len() > 5);

        // Deeply indented lists stay inside the pane too.
        let nested = format!("{}- a nested item with several words", " ".repeat(24));
        let nested = render_block(
            &block(BlockKind::User, "user_message", "You", &nested),
            40,
            &context,
            &Extra::None,
        );
        assert!(
            text(&nested).iter().all(|r| r.width() <= 40),
            "{:?}",
            text(&nested)
        );
    }

    #[test]
    fn tool_rows_pick_icons_and_show_failures() {
        let theme = Theme::new(Depth::TrueColor);
        let context = RenderContext {
            theme: &theme,
            text: Styles::new(&theme, theme.text()),
            bubble: Styles::new(&theme, theme.text()),
            expand: false,
        };
        let mut read = block(BlockKind::Tool, "dynamic_tool", "Read notes.py", "");
        read.tool_name = "Read".into();
        assert_eq!(tool_row(&read), ("◉", "Read notes.py".into()));

        let mut command = block(BlockKind::Tool, "command_execution", "$ make  (exit 2)", "");
        command.detail = "make".into();
        command.exit_code = Some(2);
        command.status = "failed".into();
        let lines = render_block(&command, 40, &context, &Extra::None);
        assert_eq!(text(&lines), vec!["❯ make  exit 2"]);
        assert_eq!(lines[0].spans[0].style.fg, Some(theme.error_fg));

        let mut approval = block(
            BlockKind::Request,
            "approval_request",
            "Approval requested: command",
            "Run the tests",
        );
        approval.detail = "Run the tests".into();
        let lines = render_block(&approval, 60, &context, &Extra::Decision("Approved".into()));
        let row = text(&lines).remove(0);
        assert!(
            row.starts_with("◈ Command approval · Run the tests"),
            "{row}"
        );
        assert!(row.ends_with("Approved"), "{row}");
        assert_eq!(row.width(), 60);
    }

    #[test]
    fn long_requests_scroll_their_text_and_keep_the_choices() {
        let panel = || RequestPanel {
            key: "q1".into(),
            title: vec![Span::raw("◈ Command approval")],
            text: (1..=30).map(|n| Line::raw(format!("line {n}"))).collect(),
            choices: vec![Line::raw("Approve")],
        };
        let mut scroll = 0;
        let rows = text(&lay_out_panel(panel(), 60, 8, &mut scroll, Style::new()));
        assert_eq!(rows.len(), 8);
        assert!(rows[0].starts_with("◈ Command approval"));
        assert!(
            rows[0].ends_with("Lines 1-6 of 30 · Alt+↑/↓ scroll"),
            "{}",
            rows[0]
        );
        assert_eq!(rows[1], "line 1");
        assert_eq!(rows[7], "Approve");

        // Scrolling past the end stops at the last line.
        scroll = 100;
        let rows = text(&lay_out_panel(panel(), 60, 8, &mut scroll, Style::new()));
        assert_eq!(scroll, 24);
        assert_eq!(rows[6], "line 30");
        assert_eq!(rows[7], "Approve");

        // A panel that fits shows everything and doesn't scroll.
        let rows = text(&lay_out_panel(panel(), 60, 40, &mut scroll, Style::new()));
        assert_eq!((rows.len(), scroll), (32, 0));
        assert_eq!(rows[0], "◈ Command approval");

        assert_eq!(wrap_chars("abcdefg", 3), ["abc", "def", "g"]);
    }

    #[test]
    fn rows_pad_between_left_and_right_and_drop_an_overflowing_right() {
        let line = row(vec![Span::raw("left")], vec![Span::raw("right")], 12);
        assert_eq!(text(std::slice::from_ref(&line))[0], "left   right");
        let line = row(vec![Span::raw("left")], vec![Span::raw("a long right")], 12);
        assert_eq!(text(&[line])[0], "left        ");
        // Labels that fill the row exactly both stay.
        let line = row(vec![Span::raw("left")], vec![Span::raw("right")], 9);
        assert_eq!(text(&[line])[0], "leftright");
        assert_eq!(fit("abcdef", 4), "abc…");
    }
}
