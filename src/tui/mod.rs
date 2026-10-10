//! The interactive client. It draws only after input or a server event, at most 30 times a second.
//! Two timers can wake it besides: a one-second tick, armed while the open thread has a run
//! going or a sidebar card on screen reads Working or Goal, so the elapsed-time labels and
//! spinners advance, and a one-shot timer for the moment the soonest snooze ends, which no
//! server event marks. An idle TUI uses no CPU.

mod composer;
mod markdown;
mod picker;
mod plan;
mod scroll;
mod sidebar;
mod tasks;
mod theme;
mod unsent;

use std::collections::{HashMap, HashSet, VecDeque};
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
use crate::models::Choice;
use crate::projection::{Applied, ShellState, ThreadState, is_active_status, status};
use crate::settings::Settings;
use crate::transcript::{self, BlockKind};
use composer::Composer;
use markdown::Styles;
use picker::{Item, Kind, Pick};
use scroll::Scroll;
use sidebar::{Capabilities, Sidebar, View};
use theme::{
    Theme, duration_label, model_display_name, monogram, now_ms, parse_iso_ms, runtime_mode_label,
};
use unsent::{Failed, Unsent};

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

/// Wrapped lines for one transcript block, valid for one width and content version.
struct Cached {
    key: (u64, u16),
    lines: Vec<Line<'static>>,
    /// The header row and the button's row of a proposed plan long enough to collapse.
    toggle: Option<(usize, usize)>,
}

/// What tools printed, for the rows verbose mode is showing. T3 leaves tool output out of a
/// thread's projection, so it is fetched one item at a time and held here, oldest evicted
/// first. Each entry is small by construction, and there are at most `MAX_ENTRIES` of them,
/// so a long thread in verbose mode cannot grow this without end.
#[derive(Default)]
struct Outputs {
    /// Item id to the version it came from and the text, which may be a note about a read
    /// that failed.
    text: HashMap<String, (String, String)>,
    /// Item ids in the order they were stored, for eviction.
    order: VecDeque<String>,
    /// Items whose output is on its way, so one row asks only once.
    fetching: HashSet<String>,
    /// Items whose read failed. They are asked for again when the thread reconnects.
    failed: HashSet<String>,
}

impl Outputs {
    /// Rows a screen can hold several times over, which is what scrolling back needs.
    const MAX_ENTRIES: usize = 64;

    /// What this version of the item printed, when it has already been fetched.
    fn get(&self, item_id: &str, revision: &str) -> Option<&str> {
        self.text
            .get(item_id)
            .filter(|(stored, _)| stored == revision)
            .map(|(_, text)| text.as_str())
    }

    /// Whether this row still has to ask. A failed read waits for a reconnect instead.
    fn wanted(&self, item_id: &str, revision: &str) -> bool {
        self.get(item_id, revision).is_none() && !self.fetching.contains(item_id)
    }

    /// Marks an item as asked for. False when a request is already out for it.
    fn start(&mut self, item_id: &str) -> bool {
        self.fetching.insert(item_id.to_string())
    }

    /// Keeps what T3 sent, or a note when it couldn't be read.
    fn store(&mut self, item_id: String, revision: String, text: Option<String>) {
        self.fetching.remove(&item_id);
        let text = match text {
            Some(text) => {
                self.failed.remove(&item_id);
                text
            }
            None => {
                self.failed.insert(item_id.clone());
                "… this output couldn't be read".into()
            }
        };
        if self
            .text
            .insert(item_id.clone(), (revision, text))
            .is_some()
        {
            self.order.retain(|stored| *stored != item_id);
        }
        self.order.push_back(item_id);
        while self.order.len() > Self::MAX_ENTRIES {
            if let Some(oldest) = self.order.pop_front() {
                self.text.remove(&oldest);
                self.failed.remove(&oldest);
            }
        }
    }

    /// Drops the reads that failed, so the rows showing them ask again.
    fn retry_failed(&mut self) {
        for item_id in self.failed.drain() {
            self.text.remove(&item_id);
            self.order.retain(|stored| *stored != item_id);
        }
    }

    fn clear(&mut self) {
        self.text.clear();
        self.order.clear();
        self.fetching.clear();
        self.failed.clear();
    }
}

/// The open thread's transcript blocks, kept from one frame to the next. An event marks the
/// turn item it changed, and the next frame describes only the marked items again, so a frame
/// drawn for a key, the clock or a resize describes none. A compaction's summary is cleaned and
/// its counts written once each time its item changes.
#[derive(Default)]
struct Prepared {
    /// In transcript order.
    blocks: Vec<transcript::Block>,
    /// The number each block was made under. It keys the block's wrapped lines in place of its
    /// text, so a frame doesn't hash a long body to find them.
    versions: Vec<u64>,
    /// How many blocks have been made, which numbers the next one.
    made: u64,
    /// Items an event changed since the blocks were made.
    changed: HashSet<String>,
    /// Whether the blocks are those of every item outside `changed`. False until the first
    /// frame, and after a snapshot, which can change or drop any item.
    current: bool,
}

impl Prepared {
    /// Marks the item an event changed. With no id to tell it by, every item counts as changed.
    fn mark(&mut self, item_id: Option<&str>) {
        match item_id {
            Some(id) => {
                self.changed.insert(id.to_string());
            }
            None => self.current = false,
        }
    }

    /// Brings the blocks up to date with `state`. The block of an item nothing has marked moves
    /// over as it was, and the blocks of items that have left the thread are dropped. An item
    /// with no row is described again, which stops at its type, its title or its blank text.
    fn refresh(&mut self, state: &ThreadState) {
        if self.current && self.changed.is_empty() {
            return;
        }
        let current = self.current;
        let mut kept: HashMap<String, (transcript::Block, u64)> = self
            .blocks
            .drain(..)
            .zip(self.versions.drain(..))
            .filter(|_| current)
            .map(|(block, version)| (block.item_id.clone(), (block, version)))
            .collect();
        for item in state.items() {
            let id = str_of(item, "id");
            let unchanged = kept.remove(id).filter(|_| !self.changed.contains(id));
            let (block, version) = match unchanged {
                Some(old) => old,
                None => match transcript::describe(item) {
                    Some(block) => {
                        self.made += 1;
                        (block, self.made)
                    }
                    None => continue,
                },
            };
            self.blocks.push(block);
            self.versions.push(version);
        }
        self.changed.clear();
        self.current = true;
    }
}

enum ActionResult {
    Info(String),
    Error(String),
    /// T3's provider and model list, from `server.getConfig`.
    Config(Value),
    /// What one tool printed, for the row that asked. `None` when T3 couldn't be reached.
    Output {
        item_id: String,
        revision: String,
        text: Option<String>,
    },
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
    /// The transcript's blocks, which go with the thread when another one opens.
    prepared: Prepared,
}

impl OpenThread {
    fn new(id: String, events: mpsc::UnboundedReceiver<WatchEvent>) -> OpenThread {
        OpenThread {
            id,
            state: None,
            events,
            connection: "connecting".into(),
            answers: Default::default(),
            run_changes: HashMap::new(),
            prepared: Prepared::default(),
        }
    }

    /// Applies one item from the thread's watch, and notes what it changed for the next frame.
    fn apply(&mut self, item: &Value) -> Applied {
        let state = self.state.get_or_insert_with(ThreadState::default);
        let applied = state.apply(item);
        let id = item.pointer("/event/payload/id").and_then(Value::as_str);
        match &applied {
            Applied::Synchronized => self.connection = "live".into(),
            Applied::Snapshot => self.prepared.mark(None),
            Applied::Event(kind) if kind.starts_with("run.") => {
                if let Some(run_id) = id {
                    self.run_changes.insert(run_id.to_string(), state.sequence);
                }
            }
            Applied::Event(kind) if kind.starts_with("turn-item.") => self.prepared.mark(id),
            _ => {}
        }
        applied
    }

    /// Brings the transcript's blocks up to date with the items applied since the last frame.
    fn prepare(&mut self) {
        if let Some(state) = &self.state {
            self.prepared.refresh(state);
        }
    }
}

struct App {
    client: Arc<Client>,
    theme: Theme,
    config: Option<Value>,
    /// Model, effort and mode choices per thread, sent with the thread's next message.
    drafts: HashMap<String, Choice>,
    /// Messages that failed to send, until they go out or reach T3 late.
    unsent: Unsent,
    picker: Option<Picker>,
    shell: Option<ShellState>,
    shell_connection: String,
    sidebar: Sidebar,
    open: Option<OpenThread>,
    focus: Focus,
    composer: Composer,
    /// How far the transcript is scrolled, and a plan header waiting for the next frame.
    scroll: Scroll,
    /// Runs of tool calls the reader has opened, by the item id of the first call.
    open_bundles: HashSet<String>,
    /// The screen row of each run-of-calls row drawn, with the run it opens.
    bundle_rows: Vec<(u16, String)>,
    /// Proposed plans the reader has expanded, by item id. Opening another thread forgets them.
    expanded_plans: HashSet<String>,
    /// Each plan long enough to collapse that the last frame drew, in transcript order.
    drawn_plans: Vec<plan::Drawn>,
    /// The tasks drawer above the composer. Only this window keeps whether it is open.
    tasks: tasks::Drawer,
    /// The item at the top of the last frame and the row of it that was showing, so a block
    /// that grows under the reader doesn't move the text.
    anchor: Option<(String, usize)>,
    /// The scroll the last frame drew, which says whether anything has moved it since.
    drawn_scroll: usize,
    /// Shows every reasoning block and tool call in full. Saved between runs.
    verbose: bool,
    /// Offers Build and Plan, from the settings file at start. Off, messages run in Build.
    plan_mode_enabled: bool,
    /// What each tool printed, for the rows verbose mode is showing.
    outputs: Outputs,
    cache: HashMap<String, Cached>,
    message: Option<(String, bool)>,
    actions: mpsc::UnboundedSender<ActionResult>,
    /// Wall-clock time of the frame being drawn, in milliseconds.
    now: i64,
    // Last drawn geometry, for mouse hit-testing and page sizes.
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
    let settings = Settings::load().await;
    let mut app = App {
        client,
        theme: Theme::detect(),
        config: None,
        drafts: HashMap::new(),
        unsent: Unsent::default(),
        picker: None,
        shell: None,
        shell_connection: "connecting".into(),
        sidebar: Sidebar::with_working(
            settings.sidebar_working_shelf_enabled,
            settings.sidebar_working_shelf_expanded,
        ),
        open: None,
        focus: Focus::Sidebar,
        composer: Composer::default(),
        scroll: Scroll::default(),
        open_bundles: HashSet::new(),
        bundle_rows: Vec::new(),
        expanded_plans: HashSet::new(),
        drawn_plans: Vec::new(),
        tasks: tasks::Drawer::default(),
        anchor: None,
        drawn_scroll: 0,
        verbose: settings.verbose,
        plan_mode_enabled: settings.plan_mode_enabled,
        outputs: Outputs::default(),
        cache: HashMap::new(),
        message: None,
        actions,
        now: now_ms(),
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
        // The sidebar's half reads the last frame drawn. A frame still waiting out the budget
        // puts it at most one frame behind.
        let running = needs_tick(app.open.as_ref(), &app.sidebar);
        if !running {
            tick_at = Instant::now() + TICK;
        }
        let tick_deadline = tokio::time::Instant::from_std(tick_at);
        // Measured from the wall clock on every pass, 50ms late like the GUI's timer, and
        // capped like its `setTimeout` delay.
        let wake = app.sidebar.next_wake;
        let wake_deadline = tokio::time::Instant::now()
            + Duration::from_millis(wake.map_or(0, |at| {
                ((at - now_ms()).max(0) + 50).min(i32::MAX as i64) as u64
            }));
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
            // Only armed while a clock or spinner moves, to advance it.
            _ = tokio::time::sleep_until(tick_deadline), if running => {
                tick_at = Instant::now() + TICK;
                dirty = true;
            }
            // Only armed while a thread is snoozed, to move it back when the snooze ends.
            _ = tokio::time::sleep_until(wake_deadline), if wake.is_some() => {
                app.rebuild_rows();
                dirty = true;
            }
        }
    }
    Ok(())
}

/// Whether a clock or spinner needs the once-a-second tick: the open thread has a run going,
/// or the last frame drew a sidebar card that reads Working or Goal. Work the sidebar doesn't
/// show, because it is hidden, settled or scrolled away, doesn't wake the TUI.
fn needs_tick(open: Option<&OpenThread>, sidebar: &Sidebar) -> bool {
    // A closed watch never hears that its run finished, so its last state can't count.
    let open_run = open
        .filter(|o| o.connection != "closed")
        .and_then(|o| o.state.as_ref())
        .is_some_and(|s| s.active_run().is_some());
    open_run || sidebar.drew_working()
}

/// The spinner frame for a wall-clock time, so every spinner on screen turns together.
fn spinner_frame(now: i64) -> &'static str {
    SPINNER[(now / 1000).rem_euclid(SPINNER.len() as i64) as usize]
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn hash(value: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
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
                // A fresh snapshot means the watch reconnected, so output T3 couldn't send
                // before is worth asking for again.
                if open.apply(&item) == Applied::Snapshot {
                    self.cache.clear();
                    self.outputs.retry_failed();
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
        self.follow_tasks();
    }

    /// Brings the tasks drawer up to date with the open thread and its watch. It runs after
    /// each change to either, so tasks that go and come back before the next frame still close
    /// the list.
    fn follow_tasks(&mut self) {
        let open = self.open.as_ref();
        self.tasks.follow(
            open.and_then(|open| open.state.as_ref()),
            open.is_some_and(|open| open.connection == "live"),
        );
    }

    /// Lays the sidebar's shelves out again from the shell, as of now.
    fn rebuild_rows(&mut self) {
        let Some(shell) = self.shell.as_ref() else {
            return;
        };
        let capabilities = self.capabilities();
        let open_id = self.open.as_ref().map(|o| o.id.as_str());
        self.sidebar
            .rebuild(&shell.threads, capabilities, open_id, now_ms());
    }

    /// Whether the server supports snooze and settlement, which decides whether those shelves
    /// apply. Discovery read the same environment descriptor that `server.getConfig` carries,
    /// so the shelves are right before the config arrives.
    fn capabilities(&self) -> Capabilities {
        Capabilities::of(match self.config.as_ref() {
            Some(config) => &config["environment"]["capabilities"],
            None => &self.client.runtime.capabilities,
        })
    }

    fn toggle_settled(&mut self) {
        self.sidebar.show_settled = !self.sidebar.show_settled;
        self.rebuild_rows();
    }

    /// Opens or closes the Working shelf and remembers it, as the GUI does. Nothing happens
    /// while the list has no Working heading.
    fn toggle_working(&mut self) {
        let Some(expanded) = self.sidebar.toggle_working() else {
            return;
        };
        Settings::update(move |settings| settings.sidebar_working_shelf_expanded = expanded);
        self.rebuild_rows();
    }

    fn shell_thread(&self, id: &str) -> Option<&Value> {
        sidebar::find_thread(self.shell.as_ref()?, id)
    }

    fn project_title(&self, project_id: &str) -> String {
        sidebar::project_title(self.shell.as_ref(), project_id)
    }

    fn open_selected(&mut self) {
        let Some(id) = self.sidebar.selected_thread_id().map(str::to_string) else {
            return;
        };
        if self.open.as_ref().is_some_and(|o| o.id == id) {
            self.focus = Focus::Composer;
            return;
        }
        let events = self.client.watch_thread(&id, None);
        self.open = Some(OpenThread::new(id, events));
        self.cache.clear();
        self.outputs.clear();
        self.open_bundles.clear();
        self.bundle_rows.clear();
        self.expanded_plans.clear();
        self.drawn_plans.clear();
        // The thread has no state yet, so the drawer closes and drops the last thread's tasks.
        self.follow_tasks();
        self.scroll.set(0);
        self.picker = None;
        self.focus = Focus::Composer;
        if let Some(open) = self.open.as_ref() {
            self.unsent.restore_saved(&mut self.composer, &open.id);
        }
        // The thread that was open may have been listed on a closed Settled shelf only
        // because it was open.
        self.rebuild_rows();
    }

    // ---- input ----

    fn on_terminal_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.on_key(key);
                true
            }
            Event::Paste(text) => {
                // An open menu covers the composer, so a paste goes to the model search or
                // nowhere.
                match self.picker.as_ref().map(|p| p.kind) {
                    Some(Kind::Model) => {
                        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
                        self.set_filter(|filter| filter.push_str(&line));
                    }
                    Some(_) => {}
                    None if self.focus == Focus::Composer => self.composer.insert_str(&text),
                    None => {}
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
                if let Some(redraw) = self.tasks.on_mouse(&mouse) {
                    return redraw;
                }
                match mouse.kind {
                    MouseEventKind::ScrollUp if inside(self.panel_area) => {
                        self.panel_scroll = self.panel_scroll.saturating_sub(1)
                    }
                    MouseEventKind::ScrollDown if inside(self.panel_area) => self.panel_scroll += 1,
                    MouseEventKind::ScrollUp if inside(self.transcript_area) => {
                        self.scroll.set(self.scroll.rows() + 3)
                    }
                    MouseEventKind::ScrollDown if inside(self.transcript_area) => {
                        self.scroll.set(self.scroll.rows().saturating_sub(3))
                    }
                    MouseEventKind::ScrollUp if inside(self.sidebar.list) => {
                        self.sidebar.move_selection(-1)
                    }
                    MouseEventKind::ScrollDown if inside(self.sidebar.list) => {
                        self.sidebar.move_selection(1)
                    }
                    MouseEventKind::Down(MouseButton::Left) if inside(self.transcript_area) => {
                        let bundle = self
                            .bundle_rows
                            .iter()
                            .find(|(y, _)| *y == mouse.row)
                            .map(|(_, id)| id.clone());
                        let card = plan::at(&self.drawn_plans, mouse.row)
                            .map(|card| (card.id.clone(), card.header));
                        match (bundle, card) {
                            (Some(id), _) => self.toggle_bundle(id),
                            (None, Some((id, header))) => self.toggle_plan(id, header),
                            // A click in the transcript is also a way to read it.
                            (None, None) => self.focus = Focus::Transcript,
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) if inside(self.sidebar.footer) => {
                        self.toggle_settled()
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if inside(self.sidebar.list)
                            && self.sidebar.working_heading_at(mouse.row) =>
                    {
                        self.toggle_working()
                    }
                    MouseEventKind::Down(MouseButton::Left) if inside(self.sidebar.list) => {
                        if let Some(id) = self.sidebar.thread_at(mouse.row).map(str::to_string)
                            && self.sidebar.select(&id)
                        {
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
            // While the tasks drawer shows, Alt+T opens and closes its list, and Alt+↑/↓ scroll
            // a list too long to show. With no drawer they go on, so Alt+T can reach `t` below.
            _ if self.tasks.on_key(&key) => return,
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
                self.scroll.set(self.scroll.rows() + self.page());
                return;
            }
            KeyCode::PageDown => {
                self.scroll
                    .set(self.scroll.rows().saturating_sub(self.page()));
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
                KeyCode::Char('u') if ctrl => {
                    self.composer.clear();
                    self.unsent.forget_composer();
                }
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
                        self.scroll.set(self.scroll.rows() + 1);
                    }
                }
                KeyCode::Down => {
                    let moved = self.composer.down();
                    if !moved {
                        self.scroll.set(self.scroll.rows().saturating_sub(1));
                    }
                }
                _ => {}
            },
            Focus::Sidebar => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Up | KeyCode::Char('k') => self.sidebar.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.sidebar.move_selection(1),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected(),
                KeyCode::Char('e') => self.toggle_settled(),
                KeyCode::Char('w') => self.toggle_working(),
                _ => {}
            },
            Focus::Transcript => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Up | KeyCode::Char('k') => self.scroll.set(self.scroll.rows() + 1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.scroll.set(self.scroll.rows().saturating_sub(1))
                }
                KeyCode::Char('g') | KeyCode::Home => self.scroll.set(usize::MAX / 2),
                KeyCode::Char('G') | KeyCode::End => self.scroll.set(0),
                KeyCode::Char('t') => self.toggle_verbose(),
                KeyCode::Char('p') => self.toggle_first_plan(),
                KeyCode::Char('a') => self.respond("accept"),
                KeyCode::Char('s') => self.respond("acceptForSession"),
                KeyCode::Char('d') => self.respond("decline"),
                KeyCode::Char('i') | KeyCode::Enter => self.focus = Focus::Composer,
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Sidebar,
                _ => {}
            },
        }
    }

    /// Opens or closes one run of tool calls, the way clicking it in the GUI would.
    fn toggle_bundle(&mut self, id: String) {
        self.focus = Focus::Transcript;
        if !self.open_bundles.remove(&id) {
            self.open_bundles.insert(id);
        }
    }

    /// Expands or collapses one proposed plan, as its button in the GUI does. Only this
    /// window remembers it: nothing goes to T3 or the settings file. The next frame keeps the
    /// card's header on the row it was on, so the click doesn't move the text, unless a key
    /// or the wheel moves the transcript before that frame.
    fn toggle_plan(&mut self, id: String, header: isize) {
        self.focus = Focus::Transcript;
        let row = plan::flip(&mut self.expanded_plans, &id, header);
        self.scroll.pin(id, row);
    }

    /// `p` expands or collapses the first long plan in view, reading down from the top of the
    /// transcript. A card counts while any of it shows, even when its header has scrolled
    /// off. With none in view, `p` does nothing.
    fn toggle_first_plan(&mut self) {
        if let Some(first) = self.drawn_plans.first() {
            let (id, header) = (first.id.clone(), first.header);
            self.toggle_plan(id, header);
        }
    }

    /// Turns verbose mode on or off and remembers it, the way the GUI keeps a view setting.
    /// It opens every run of calls at once, instead of one click at a time.
    fn toggle_verbose(&mut self) {
        self.verbose = !self.verbose;
        self.cache.clear();
        self.open_bundles.clear();
        let verbose = self.verbose;
        Settings::update(move |settings| settings.verbose = verbose);
        self.message = Some((
            if self.verbose {
                "Verbose mode on: every tool call stays open, with its output.".into()
            } else {
                "Verbose mode off: tool calls fold into one row you can click.".into()
            },
            false,
        ));
    }

    /// Asks T3 what one tool printed. The answer arrives as an action. A read that fails says
    /// so in the row rather than interrupting, and is tried again when the thread reconnects.
    fn fetch_output(&mut self, item_id: String, revision: String) {
        let Some(thread_id) = self.open.as_ref().map(|open| open.id.clone()) else {
            return;
        };
        if !self.outputs.start(&item_id) {
            return;
        }
        let (client, results) = (self.client.clone(), self.actions.clone());
        tokio::spawn(async move {
            let item = client.turn_item(&thread_id, &item_id, &revision).await;
            let _ = results.send(ActionResult::Output {
                item_id,
                revision,
                text: item.ok().map(|item| transcript::tool_output(&item)),
            });
        });
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
                // Its capabilities decide whether the Snoozed and Settled shelves apply.
                self.rebuild_rows();
            }
            ActionResult::Output {
                item_id,
                revision,
                text,
            } => {
                self.outputs.store(item_id, revision, text);
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
                let message = Failed {
                    thread_id: thread_id.clone(),
                    message_id: Some(message_id),
                    text,
                };
                self.give_back(&thread_id, message, error);
                // The thread may have shown the message before this result arrived.
                self.drop_landed();
            }
        }
    }

    /// A send that timed out can still reach T3. Once the open thread shows one, the copy kept
    /// for a retry comes out, so it isn't sent twice.
    fn drop_landed(&mut self) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let Some(state) = open.state.as_ref() else {
            return;
        };
        if let Some(note) = self
            .unsent
            .drop_landed(&mut self.composer, &open.id, |id| state.has_message(id))
        {
            self.message = Some(note);
        }
    }

    /// Puts a failed message back in the composer. If the composer holds other text, or the
    /// message was for another thread, it waits for Ctrl+R or for that thread.
    fn give_back(&mut self, thread_id: &str, message: Failed, error: String) {
        if self.open.as_ref().is_some_and(|o| o.id == thread_id) && self.composer.is_empty() {
            self.unsent.restore(&mut self.composer, vec![message]);
            self.message = Some((error, true));
            return;
        }
        self.unsent.save(thread_id, message);
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
        if self.unsent.swap(&mut self.composer, &id) {
            self.focus = Focus::Composer;
        } else {
            self.message = Some(("This thread has no unsent message.".into(), false));
        }
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
        if matches!(
            picker::spent(config, thread, draft, self.plan_mode_enabled),
            Ok(true)
        ) {
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
        Some(picker::view(
            self.config.as_ref(),
            thread,
            draft,
            self.plan_mode_enabled,
        ))
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
        match picker::spent(config, thread, &draft, self.plan_mode_enabled) {
            Err(e) => self.message = Some((e.to_string(), true)),
            Ok(true) => {
                self.drafts.remove(&id);
            }
            Ok(false) => {
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
                self.unsent.forget_composer();
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

        // The draft goes out with this message and stays until the thread shows it. A check
        // that fails here leaves the text in the composer.
        let thread_id = open.id.clone();
        let empty = Choice::default();
        let draft = self.drafts.get(&thread_id).unwrap_or(&empty);
        let plan = match picker::send_plan(
            self.config.as_ref(),
            state.thread(),
            draft,
            self.plan_mode_enabled,
        ) {
            Ok(plan) => plan,
            Err(e) => {
                self.message = Some((e.to_string(), true));
                return;
            }
        };
        // The client refuses this too, but its message names the run id. Alt+P can't undo a
        // switch from Plan to Build where Plan isn't offered, so that message leaves it out.
        if plan.changes_modes() && state.active_run().is_some() {
            let selection = plan
                .model_selection
                .as_ref()
                .unwrap_or(&state.thread()["modelSelection"]);
            let error = if plan.interaction_mode.is_some()
                && !picker::offers_plan(self.config.as_ref(), selection, self.plan_mode_enabled)
            {
                "This message would move the thread from Plan to Build, and T3 can't change the mode during a run. Send again when it finishes."
            } else {
                "T3 can't change the mode during a run. Send again when it finishes, or set the mode back with Alt+P."
            };
            self.message = Some((error.into(), true));
            return;
        }
        self.composer.clear();
        self.unsent.forget_composer();
        self.scroll.set(0);
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
        spinner_frame(self.now)
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
        // The wake timer usually moves a woken thread first, but it runs on a clock that
        // stops while the machine sleeps.
        if self.sidebar.next_wake.is_some_and(|wake| wake <= self.now) {
            self.rebuild_rows();
        }
        let (_, dot) = self.connection_state();
        let view = View {
            theme: &self.theme,
            shell: self.shell.as_ref(),
            open_id: self.open.as_ref().map(|o| o.id.as_str()),
            focused: self.focus == Focus::Sidebar,
            now: self.now,
            dot,
        };
        self.sidebar.draw(frame, sidebar, &view);

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
        // The tasks drawer takes what room the panel leaves. A waiting request hides it, so the
        // two never show together.
        let tasks_room = main
            .height
            .saturating_sub(2 + 3 + composer_height + 1 + panel_height);
        let tasks = self.tasks.lay_out(
            main.width.saturating_sub(4) as usize,
            tasks_room as usize,
            area.height as usize,
            &self.theme,
        );
        let [header, body, panel_area, drawer, composer_area, status_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(panel_height),
            Constraint::Length(tasks.len() as u16),
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
        // A tab on the composer's top edge, a column in from its corners, with the composer's
        // background and its text in line with the composer's.
        self.tasks.area = Rect {
            x: drawer.x + 1,
            width: drawer.width.saturating_sub(2),
            ..drawer
        };
        if !tasks.is_empty() {
            let raised = Block::new().style(Style::new().bg(self.theme.raised));
            frame.render_widget(raised, self.tasks.area);
            let inner = Rect {
                x: drawer.x + 2,
                width: drawer.width.saturating_sub(4),
                ..drawer
            };
            frame.render_widget(Paragraph::new(tasks), inner);
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
        // Clicks and `p` act on the plans this frame draws, so the last frame's go first.
        self.drawn_plans.clear();
        if let Some(open) = self.open.as_mut() {
            open.prepare();
        }
        let Some((state, run_changes, prepared)) = self
            .open
            .as_ref()
            .and_then(|o| Some((o.state.as_ref()?, &o.run_changes, &o.prepared)))
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
        let blocks = &prepared.blocks;
        // A plan that has left the thread forgets that it was expanded.
        plan::keep_present(&mut self.expanded_plans, blocks);
        let active_run = state.active_run().map(|r| str_of(r, "id").to_string());
        let context = RenderContext {
            theme: &t,
            text: Styles::new(&t, t.text()),
            bubble: Styles::new(&t, Style::new().fg(t.fg).bg(t.bubble)),
            reasoning: Styles::new(&t, Style::new().fg(t.muted)).dimmed(),
        };
        // Tool calls are shown in runs: one row for the lot, opened by clicking it or by
        // verbose mode. Everything else, reasoning included, is always there.
        let bundles = bundle_tools(blocks);
        // Each block keeps its wrapped lines until it is made again or the width changes. Only
        // a streaming block, or the clock row under a running prompt, is rewrapped per frame.
        let mut heights = Vec::with_capacity(blocks.len());
        for (index, block) in blocks.iter().enumerate() {
            let bundle = bundles[index].as_ref().map(|head| {
                let open = self.verbose || self.open_bundles.contains(&head.id);
                (head, open)
            });
            // A closed run of calls is drawn by its first block alone.
            if let Some((head, open)) = &bundle
                && !open
                && head.id != block.item_id
            {
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
            // Tool output T3 withheld, for the rows that have already been given it.
            let output = self.outputs.get(&block.item_id, &block.updated_at);
            // The row this block stands for when it heads a run of calls, which changes as
            // calls are added to it and when it is opened.
            let head = match &bundle {
                Some((head, open)) if head.id == block.item_id => {
                    format!("{}{}{}", open, head.count, head.names)
                }
                _ => String::new(),
            };
            // A proposed plan draws its preview or all of it, so which one joins the key.
            let expanded =
                block.item_type == "proposed_plan" && self.expanded_plans.contains(&block.item_id);
            // Everything a row is drawn from. The block's number stands for all it took from its
            // item, so a tool that keeps its header but changes its description, its argument
            // or how it ended still redraws.
            let key = (
                hash((
                    prepared.versions[index],
                    output,
                    &head,
                    clock,
                    &decision,
                    expanded,
                    stamp,
                )),
                width,
            );
            let fresh = self.cache.get(&block.item_id).is_none_or(|c| c.key != key);
            if fresh && block.item_type == "proposed_plan" {
                let card = plan::card(&block.body, width as usize, &context, expanded);
                let cached = Cached {
                    key,
                    lines: card.lines,
                    toggle: card.toggle,
                };
                self.cache.insert(block.item_id.clone(), cached);
            } else if fresh {
                let extra = match (block.kind, &bundle) {
                    (BlockKind::User, _) => self
                        .fold_for(state, &block.run_id)
                        .map_or(Extra::None, Extra::Fold),
                    (BlockKind::Request, _) => Extra::Decision(decision),
                    (BlockKind::Tool, Some((head, open))) if head.id == block.item_id => {
                        Extra::Bundle(Bundle {
                            count: head.count,
                            names: head.names.clone(),
                            open: *open,
                        })
                    }
                    _ => Extra::None,
                };
                let lines = match output {
                    Some(text) => {
                        let block = transcript::Block {
                            body: text.to_string(),
                            ..block.clone()
                        };
                        render_block(&block, width as usize, &context, &extra)
                    }
                    None => render_block(block, width as usize, &context, &extra),
                };
                let cached = Cached {
                    key,
                    lines,
                    toggle: None,
                };
                self.cache.insert(block.item_id.clone(), cached);
            }
            heights.push(self.cache[&block.item_id].lines.len());
        }
        let total: usize = heights.iter().sum();
        let height = area.height as usize;
        // A plan just expanded or collapsed keeps its header on the row it had, so the click
        // doesn't move the text, even while the view follows the bottom. A key or the wheel
        // that moved the transcript after the toggle has already cancelled this.
        let pinned = self.scroll.take_pin().and_then(|(id, row)| {
            let header = self.cache.get(&id)?.toggle?.0;
            scroll_to(blocks, &heights, &id, header, row, height)
        });
        // Scrolled up, the view stays on the row it was reading: output that arrives for a
        // block on screen makes it taller, and without this the text would slide away. The
        // anchor is dropped as soon as anything else moves the scroll, so keys and the wheel
        // still win, and following the bottom is untouched.
        let anchored = if self.scroll.rows() > 0 && self.scroll.rows() == self.drawn_scroll {
            self.anchor.as_ref().and_then(|(item_id, within)| {
                scroll_to(blocks, &heights, item_id, *within, 0, height)
            })
        } else {
            None
        };
        if let Some(scroll) = pinned.or(anchored) {
            self.scroll.set(scroll);
        }
        let scroll = self.scroll.rows().min(total.saturating_sub(height));
        self.scroll.set(scroll);
        let end = total - scroll;
        let start = end.saturating_sub(height);

        // Copy only the rows in view.
        let mut lines = Vec::with_capacity(height);
        let mut offset = 0;
        // Verbose mode shows what a tool printed, which T3 hands over one item at a time. Only
        // the rows on screen ask for it, so opening a long thread doesn't fetch all of it.
        let mut wanted = Vec::new();
        let mut anchor = None;
        let mut rows = Vec::new();
        let mut plans = Vec::new();
        for ((block, block_height), bundle) in blocks.iter().zip(&heights).zip(&bundles) {
            if *block_height > 0 && offset + block_height > start && offset < end {
                let cached = &self.cache[&block.item_id];
                let from = start.saturating_sub(offset);
                let to = (end - offset).min(*block_height);
                // The row a run of calls is drawn as, so a click on it can open the run.
                if let Some(head) = bundle
                    && head.id == block.item_id
                    && from == 0
                {
                    rows.push((area.y + lines.len() as u16, head.id.clone()));
                }
                // A long plan in view, whose header and button a click or `p` toggles.
                if let Some(toggle) = cached.toggle
                    && let Some(card) =
                        plan::drawn(&block.item_id, toggle, from, to, lines.len(), area.y)
                {
                    plans.push(card);
                }
                lines.extend(cached.lines[from..to].iter().cloned());
                anchor.get_or_insert_with(|| (block.item_id.clone(), from));
                if block.output_omitted && self.outputs.wanted(&block.item_id, &block.updated_at) {
                    wanted.push((block.item_id.clone(), block.updated_at.clone()));
                }
            }
            offset += block_height;
            if offset >= end {
                break;
            }
        }
        self.bundle_rows = rows;
        self.drawn_plans = plans;
        frame.render_widget(Paragraph::new(lines), area);
        if scroll > 0 {
            let label = format!(" ↓ {scroll} more ");
            let x = area.x + area.width.saturating_sub(label.width() as u16 + 1);
            frame.render_widget(
                Paragraph::new(Line::styled(
                    label.clone(),
                    Style::new().fg(t.fg).bg(t.primary),
                )),
                Rect::new(x, area.y + area.height - 1, label.width() as u16, 1),
            );
        }
        self.anchor = anchor.filter(|_| scroll > 0);
        self.drawn_scroll = scroll;
        for (item_id, revision) in wanted {
            self.fetch_output(item_id, revision);
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
        Some(Fold { label, tone })
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
            .is_some_and(|o| self.unsent.has_saved(&o.id));
        let hint;
        let keys = match (self.focus, self.picker.as_ref().map(|p| p.kind)) {
            (_, Some(Kind::Model)) => "Type to search · ↑↓ choose · Enter select · Esc close",
            (_, Some(_)) => "↑↓ choose · Enter select · Esc close",
            (Focus::Sidebar, None) if self.sidebar.has_working() => {
                "↑↓ select · Enter open · w working · e settled · Tab focus · q quit"
            }
            (Focus::Sidebar, None) => "↑↓ select · Enter open · e settled · Tab focus · q quit",
            (Focus::Composer, None) => {
                "Enter send · Alt+Enter newline · Alt+M model · Alt+E effort · Alt+P mode · Ctrl+X interrupt"
            }
            (Focus::Transcript, None) => {
                let plan = self
                    .drawn_plans
                    .first()
                    .map(|card| self.expanded_plans.contains(&card.id));
                hint = transcript_keys(self.verbose, plan);
                hint.as_str()
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

/// The keys the status line offers while the transcript has focus. `plan` is whether the
/// first long plan in view, the one `p` toggles, is expanded, when there is one.
fn transcript_keys(verbose: bool, plan: Option<bool>) -> String {
    let plan = match plan {
        Some(true) => " · p collapse plan",
        Some(false) => " · p expand plan",
        None => "",
    };
    let calls = if verbose {
        "t close them"
    } else {
        "t open all"
    };
    format!("↑↓/PgUp scroll · G bottom{plan} · click a row of calls · {calls} · Esc sidebar")
}

/// The scroll, in rows up from the bottom, that puts row `within` of block `id` on screen row
/// `row`, which is negative above the screen. None when the block isn't in the thread.
fn scroll_to(
    blocks: &[transcript::Block],
    heights: &[usize],
    id: &str,
    within: usize,
    row: isize,
    height: usize,
) -> Option<usize> {
    let total: usize = heights.iter().sum();
    let mut above = 0;
    for (block, block_height) in blocks.iter().zip(heights) {
        if block.item_id == id {
            let top = above + within.min(block_height.saturating_sub(1));
            let start = top.saturating_add_signed(-row);
            return Some(total.saturating_sub(start + height));
        }
        above += block_height;
    }
    None
}

// ---- block rendering ----

struct RenderContext<'a> {
    theme: &'a Theme,
    text: Styles,
    bubble: Styles,
    /// Reasoning prose, dimmer than the model's answers.
    reasoning: Styles,
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
    /// The head of a run of tool calls, which is one row until it is opened.
    Bundle(Bundle),
}

/// A run of tool calls shown as one row. Clicking it opens the calls and their output.
struct Bundle {
    /// How many calls the row stands for.
    count: usize,
    /// What they were, such as `Read, Run`, so a closed row still says what happened.
    names: String,
    open: bool,
}

/// The head of a run of tool calls: the block that draws the row, and what it stands for.
#[derive(Clone)]
struct BundleHead {
    /// The item id of the first call, which names the run.
    id: String,
    count: usize,
    names: String,
}

/// Groups neighbouring tool calls from one run, so each group is one row in the transcript.
/// The result lines up with `blocks`: every call in a group points at the same head.
fn bundle_tools(blocks: &[transcript::Block]) -> Vec<Option<BundleHead>> {
    let mut heads: Vec<Option<BundleHead>> = vec![None; blocks.len()];
    let mut start = 0;
    while start < blocks.len() {
        if blocks[start].kind != BlockKind::Tool {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < blocks.len()
            && blocks[end].kind == BlockKind::Tool
            && blocks[end].run_id == blocks[start].run_id
        {
            end += 1;
        }
        // Each tool named once, in the order they ran: `Read, Run` reads better than
        // `Read, Read, Run, Read`.
        let mut names: Vec<String> = Vec::new();
        for block in &blocks[start..end] {
            let name = tool_row(block).1;
            if !names.contains(&name) {
                names.push(name);
            }
        }
        let head = BundleHead {
            id: blocks[start].item_id.clone(),
            count: end - start,
            names: names.join(", "),
        };
        for slot in &mut heads[start..end] {
            *slot = Some(head.clone());
        }
        start = end;
    }
    heads
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

/// The icon, action and argument for a tool row, after the GUI's
/// Eye/SquarePen/Terminal/Globe/Search icon set. The argument is drawn as a chip after the
/// action, so a long command or path can be cut without hiding what the tool did.
fn tool_row(block: &transcript::Block) -> (&'static str, String, String) {
    // T3 describes some items itself, as the GUI shows them. Its description beats a label
    // built from the item type.
    let described = |fallback: &str| {
        if block.title.is_empty() || block.title == block.detail {
            fallback.to_string()
        } else {
            block.title.clone()
        }
    };
    match block.item_type.as_str() {
        "command_execution" => ("❯", described("Run"), block.detail.clone()),
        "file_change" => (
            "✎",
            described("Edit"),
            block.header.trim_start_matches("edit ").to_string(),
        ),
        "file_search" => ("⌕", described("Search"), block.detail.clone()),
        "web_search" => ("◎", described("Web search"), block.detail.clone()),
        "subagent" => ("⧉", block.header.clone(), block.detail.clone()),
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
            // The tool's own name is the action; its input is the argument. T3's title
            // repeats both, so it is only the fallback.
            let action = if block.tool_name.is_empty() {
                block.header.clone()
            } else {
                block.tool_name.clone()
            };
            (icon, action, block.detail.clone())
        }
        _ => ("⚙", block.header.clone(), block.detail.clone()),
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
                lines.push(Line::styled(fold.label.clone(), Style::new().fg(color)));
                lines.push(Line::styled("─".repeat(width), hairline));
            }
        }
        BlockKind::Assistant => {
            lines.push(Line::default());
            lines.extend(markdown::render(block.body.trim(), width, &context.text, 0));
            lines.push(Line::default());
        }
        BlockKind::Reasoning => {
            let label = if block.streaming {
                "Thinking"
            } else {
                "Thought"
            };
            // Reasoning always reads out in full, muted so the model's own answers stay the
            // brightest text on screen. Tool rows pack together; a block of prose gets air.
            lines.push(Line::default());
            lines.push(Line::styled(format!("✦ {label}"), muted));
            lines.extend(markdown::render(
                block.body.trim(),
                width,
                &context.reasoning,
                0,
            ));
            lines.push(Line::default());
        }
        BlockKind::Tool => {
            // A run of tool calls is one row until the reader asks for it. The row says how
            // many there were and which tools ran, so a closed turn still reads.
            if let Extra::Bundle(bundle) = extra {
                let calls = match bundle.count {
                    1 => "1 tool call".to_string(),
                    count => format!("{count} tool calls"),
                };
                let mut spans = vec![
                    Span::styled(
                        format!("{} ", if bundle.open { "⌄" } else { "›" }),
                        Style::new().fg(t.border_strong),
                    ),
                    Span::styled(calls.clone(), Style::new().fg(t.muted)),
                ];
                let room = width.saturating_sub(calls.width() + 5);
                if !bundle.names.is_empty() && room > 4 {
                    spans.push(Span::styled(
                        format!(" · {}", fit(&bundle.names, room)),
                        Style::new().fg(t.border_strong),
                    ));
                }
                lines.push(Line::from(spans));
                if !bundle.open {
                    return lines;
                }
            }
            let (icon, label, argument) = tool_row(block);
            let color = match block.status.as_str() {
                "running" | "pending" | "inProgress" | "in_progress" => t.info,
                "failed" | "error" => t.error_fg,
                _ => t.muted,
            };
            let exit = block.exit_code.filter(|code| *code != 0);
            let mut room = width.saturating_sub(2 + exit.map_or(0, |_| 10));
            // The action keeps at most half the row, so the argument always has space.
            let label = fit(&label, room.min((width / 2).max(12)));
            room = room.saturating_sub(label.width());
            let mut spans = vec![Span::styled(
                format!("{icon} {label}"),
                Style::new().fg(color),
            )];
            // The argument rides in a chip, as in the GUI, where a path or command is set off
            // from the words around it.
            let argument = argument.trim();
            if !argument.is_empty() && room > 6 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!(" {} ", fit(argument, room - 4)),
                    Style::new().fg(t.fg).bg(t.chip_bg),
                ));
            }
            if let Some(code) = exit {
                spans.push(Span::styled(
                    format!("  exit {code}"),
                    Style::new().fg(t.error_fg),
                ));
            }
            lines.push(Line::from(spans));
            if !block.body.is_empty() {
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
            let compaction = block.item_type == "compaction";
            // A compaction still under way has its label in blue, where the desktop shimmers
            // it, so the row changes only when the item does.
            let label = match block.status.as_str() {
                "pending" | "running" | "waiting" if compaction => Style::new().fg(t.info),
                _ => muted,
            };
            lines.push(Line::default());
            let title = fit(&block.header, width.saturating_sub(6));
            let side = width.saturating_sub(title.width() + 2) / 2;
            let rest = width.saturating_sub(side + title.width() + 2);
            lines.push(Line::from(vec![
                Span::styled("─".repeat(side), hairline),
                Span::styled(format!(" {title} "), label),
                Span::styled("─".repeat(rest), hairline),
            ]));
            // Of the notices, only a compaction has detail: the counts its label leaves out
            // and the summary, which `describe` has already bounded and cleaned.
            if compaction && !block.body.is_empty() {
                for text in tasks::wrap(&block.body, width.saturating_sub(4)) {
                    lines.push(Line::styled(format!("  {text}"), muted));
                }
            }
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
            title: header.into(),
            output_omitted: false,
            updated_at: "2026-10-07T00:00:00.000Z".into(),
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
            reasoning: Styles::new(&theme, Style::new().fg(theme.muted)).dimmed(),
        };
        let fold = Extra::Fold(Fold {
            label: "Worked for 2m 32s".into(),
            tone: Tone::Muted,
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
        assert_eq!(rows[4], "Worked for 2m 32s");
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
    fn fetched_output_is_bounded_and_a_failed_read_waits_for_the_reconnect() {
        let mut outputs = Outputs::default();
        // A row asks once, and what comes back belongs to that version of the item.
        assert!(outputs.wanted("a", "v1"));
        assert!(outputs.start("a"));
        assert!(!outputs.start("a"), "a request is already out");
        assert!(!outputs.wanted("a", "v1"));
        outputs.store("a".into(), "v1".into(), Some("done".into()));
        assert_eq!(outputs.get("a", "v1"), Some("done"));
        // The tool ran again, so its output is a new question.
        assert_eq!(outputs.get("a", "v2"), None);
        assert!(outputs.wanted("a", "v2"));

        // A read that failed says so in the row and is not asked again until the thread
        // reconnects, so a dropped socket can't leave a row empty for good.
        outputs.store("b".into(), "v1".into(), None);
        assert!(
            outputs
                .get("b", "v1")
                .is_some_and(|t| t.contains("couldn't"))
        );
        assert!(!outputs.wanted("b", "v1"));
        outputs.retry_failed();
        assert!(outputs.wanted("b", "v1"));
        assert_eq!(outputs.get("a", "v1"), Some("done"), "a good read is kept");

        // Only so many are held, the oldest dropped first.
        for n in 0..Outputs::MAX_ENTRIES {
            outputs.store(format!("item{n}"), "v1".into(), Some("x".into()));
        }
        assert_eq!(outputs.text.len(), Outputs::MAX_ENTRIES);
        assert_eq!(outputs.order.len(), Outputs::MAX_ENTRIES);
        assert_eq!(outputs.get("a", "v1"), None, "the oldest went first");
    }

    #[test]
    fn reasoning_reads_out_in_full_and_stays_dimmer_than_an_answer() {
        let theme = Theme::new(Depth::TrueColor);
        let context = RenderContext {
            theme: &theme,
            text: Styles::new(&theme, theme.text()),
            bubble: Styles::new(&theme, theme.text()),
            reasoning: Styles::new(&theme, Style::new().fg(theme.muted)).dimmed(),
        };
        // Code and links carry colors of their own, which must not make reasoning bright.
        let body =
            "First I check the tests.\n\n```sh\ncargo test\n```\n\nThen [they](http://x) run.";
        let thought = block(BlockKind::Reasoning, "reasoning", "Thinking", body);

        let full = render_block(&thought, 40, &context, &Extra::None);
        let rows = text(&full).join("\n");
        assert!(rows.contains("First I check the tests."), "{rows}");
        assert!(rows.contains("cargo test"), "{rows}");
        assert!(rows.contains("Then they (http://x) run."), "{rows}");
        assert_ne!(theme.muted, theme.fg, "reasoning must not match answers");
        for span in full.iter().flat_map(|line| &line.spans) {
            assert_ne!(
                span.style.fg,
                Some(theme.fg),
                "{:?} is as bright as an answer",
                span.content
            );
        }
        for word in ["cargo test", "they"] {
            let span = full
                .iter()
                .flat_map(|line| &line.spans)
                .find(|span| span.content.contains(word))
                .expect("the reasoning text");
            assert_eq!(span.style.fg, Some(theme.muted), "{word}");
        }
    }

    #[test]
    fn neighbouring_tool_calls_become_one_row_until_it_is_opened() {
        let theme = Theme::new(Depth::TrueColor);
        let context = RenderContext {
            theme: &theme,
            text: Styles::new(&theme, theme.text()),
            bubble: Styles::new(&theme, theme.text()),
            reasoning: Styles::new(&theme, Style::new().fg(theme.muted)).dimmed(),
        };
        let tool = |id: &str, name: &str, run: &str| {
            let mut call = block(BlockKind::Tool, "dynamic_tool", name, "the output");
            call.item_id = id.into();
            call.tool_name = name.into();
            call.detail = "notes.py".into();
            call.run_id = run.into();
            call
        };
        let blocks = vec![
            tool("a", "Read", "run1"),
            tool("b", "Read", "run1"),
            tool("c", "Grep", "run1"),
            block(BlockKind::Assistant, "assistant_message", "T3", "Done."),
            tool("d", "Read", "run2"),
        ];
        let heads = bundle_tools(&blocks);

        // The three calls of the first run share a head; the one after the answer is its own.
        let ids: Vec<Option<&str>> = heads
            .iter()
            .map(|head| head.as_ref().map(|h| h.id.as_str()))
            .collect();
        assert_eq!(ids, vec![Some("a"), Some("a"), Some("a"), None, Some("d")]);
        let head = heads[0].clone().expect("the first run of calls");
        assert_eq!(head.count, 3);
        assert_eq!(head.names, "Read, Grep", "each tool named once, in order");

        // Closed, the head draws one row for the run and the calls themselves draw nothing.
        let closed = render_block(
            &blocks[0],
            60,
            &context,
            &Extra::Bundle(Bundle {
                count: head.count,
                names: head.names.clone(),
                open: false,
            }),
        );
        assert_eq!(text(&closed), vec!["› 3 tool calls · Read, Grep"]);

        // Opened, the same row is followed by the call and what it printed.
        let open = render_block(
            &blocks[0],
            60,
            &context,
            &Extra::Bundle(Bundle {
                count: head.count,
                names: head.names,
                open: true,
            }),
        );
        let rows = text(&open);
        assert_eq!(rows[0], "⌄ 3 tool calls · Read, Grep");
        assert!(rows[1].starts_with("◉ Read"), "{rows:?}");
        assert!(rows[2].contains("│ the output"), "{rows:?}");
    }

    #[test]
    fn tool_rows_pick_icons_and_show_failures() {
        let theme = Theme::new(Depth::TrueColor);
        let context = RenderContext {
            theme: &theme,
            text: Styles::new(&theme, theme.text()),
            bubble: Styles::new(&theme, theme.text()),
            reasoning: Styles::new(&theme, Style::new().fg(theme.muted)).dimmed(),
        };
        // The tool's name is the action and its input the argument, so a long path can be
        // cut without hiding which tool ran.
        let mut read = block(BlockKind::Tool, "dynamic_tool", "Read notes.py", "");
        read.tool_name = "Read".into();
        read.detail = "/tmp/demo/notes.py".into();
        assert_eq!(
            tool_row(&read),
            ("◉", "Read".into(), "/tmp/demo/notes.py".into())
        );
        let lines = render_block(&read, 40, &context, &Extra::None);
        assert_eq!(text(&lines), vec!["◉ Read   /tmp/demo/notes.py "]);
        // The argument sits in a chip, which the action beside it does not.
        assert_eq!(lines[0].spans[2].style.bg, Some(theme.chip_bg));
        assert_eq!(lines[0].spans[0].style.bg, None);

        // T3's own description of a command becomes the action, with the command as argument.
        let mut command = block(BlockKind::Tool, "command_execution", "$ make  (exit 2)", "");
        command.detail = "make".into();
        command.title = "Build the project".into();
        command.exit_code = Some(2);
        command.status = "failed".into();
        let lines = render_block(&command, 40, &context, &Extra::None);
        assert_eq!(text(&lines), vec!["❯ Build the project   make   exit 2"]);
        assert_eq!(lines[0].spans[0].style.fg, Some(theme.error_fg));

        // Without a description the item type names the action.
        command.title = String::new();
        assert_eq!(tool_row(&command).1, "Run");

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

    #[test]
    fn the_tick_follows_the_open_run_and_the_working_cards_drawn() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let theme = Theme::new(Depth::TrueColor);
        // `sidebar` as the event loop holds it after drawing a frame of `threads`.
        let drawn_by = |mut sidebar: Sidebar, threads: Vec<Value>| {
            let shell = ShellState {
                sequence: 1,
                projects: Vec::new(),
                threads,
                synchronized: true,
            };
            sidebar.rebuild(&shell.threads, Capabilities::default(), None, now_ms());
            let view = View {
                theme: &theme,
                shell: Some(&shell),
                open_id: None,
                focused: false,
                now: now_ms(),
                dot: theme.success,
            };
            let mut terminal = Terminal::new(TestBackend::new(30, 12)).unwrap();
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    sidebar.draw(frame, area, &view);
                })
                .unwrap();
            sidebar
        };
        let drawn = |threads: Vec<Value>| drawn_by(Sidebar::default(), threads);
        let thread = |id: &str, lineage: Value| {
            json!({
                "id": id,
                "projectId": "p",
                "title": id,
                "latestRunId": "r",
                "status": "running",
                "lineage": lineage,
            })
        };
        // The open thread `o`, with one run in `status`. It has no card in these sidebars.
        let open = |connection: &str, status: &str| OpenThread {
            id: "o".into(),
            state: ThreadState::from_snapshot(&json!({
                "snapshotSequence": 1,
                "projection": {
                    "thread": {"id": "o"},
                    "runs": [{"id": "r", "status": status, "ordinal": 1}],
                },
            })),
            events: mpsc::unbounded_channel().1,
            connection: connection.into(),
            answers: serde_json::Map::new(),
            run_changes: HashMap::new(),
            prepared: Prepared::default(),
        };

        // A working card on screen ticks by itself, even past a closed watch.
        let busy = drawn(vec![thread("busy", json!({}))]);
        assert!(busy.drew_working());
        assert!(needs_tick(None, &busy));
        assert!(needs_tick(Some(&open("closed", "running")), &busy));

        // A subagent works with no card, so it needs no tick. The open thread's run still
        // ticks the transcript's clock while its watch is open.
        let hidden = drawn(vec![thread(
            "sub",
            json!({"relationshipToParent": "subagent"}),
        )]);
        assert!(!hidden.drew_working());
        assert!(!needs_tick(None, &hidden));
        assert!(needs_tick(Some(&open("live", "running")), &hidden));
        assert!(needs_tick(
            Some(&open("reconnecting: timeout", "waiting")),
            &hidden
        ));
        // A closed watch never hears the run finish, so its last state doesn't count. A
        // finished run has no clock to move.
        assert!(!needs_tick(Some(&open("closed", "running")), &hidden));
        assert!(!needs_tick(Some(&open("live", "completed")), &hidden));

        // A closed Working shelf hides the busy card behind its heading, so it needs no tick.
        // Open, the card shows and ticks.
        let closed = drawn_by(
            Sidebar::with_working(true, false),
            vec![thread("busy", json!({}))],
        );
        assert!(!closed.drew_working());
        assert!(!needs_tick(None, &closed));
        let open_shelf = drawn_by(
            Sidebar::with_working(true, true),
            vec![thread("busy", json!({}))],
        );
        assert!(open_shelf.drew_working());
    }

    #[test]
    fn a_toggled_plan_keeps_its_header_on_its_row() {
        let blocks: Vec<transcript::Block> = ["a", "plan", "c"]
            .into_iter()
            .map(|id| {
                let mut block = block(BlockKind::Plan, "proposed_plan", "Proposed plan", "");
                block.item_id = id.into();
                block
            })
            .collect();
        // The card's header is its row 1, so transcript row 11. Twenty rows showing the bottom
        // start at row 7, which puts the header on screen row 4.
        let collapsed = [10, 12, 5];
        let expanded = [10, 40, 5];
        // Expanded, the header stays on row 4 and the view leaves the bottom.
        let scroll = scroll_to(&blocks, &expanded, "plan", 1, 4, 20).expect("the plan");
        assert_eq!(scroll, 28);
        assert_eq!(55 - scroll - 20, 7, "the same rows above the header");
        // Collapsed again from there, the view is back at the bottom.
        assert_eq!(scroll_to(&blocks, &collapsed, "plan", 1, 4, 20), Some(0));

        // Five rows showing rows 20..25, inside the collapsed card: the header is 9 rows above
        // the screen. Expanding keeps it there, so the same rows stay in view.
        let mut plans = HashSet::new();
        let row = plan::flip(&mut plans, "plan", -9);
        let scroll = scroll_to(&blocks, &expanded, "plan", 1, row, 5).expect("the plan");
        assert_eq!(55 - scroll - 5, 20);
        // Collapsing from the bottom of the expanded card, its header 24 rows up, brings the
        // header to the top row rather than leaving the shorter card above the screen.
        let row = plan::flip(&mut plans, "plan", -24);
        assert_eq!(row, 0);
        let scroll = scroll_to(&blocks, &collapsed, "plan", 1, row, 5).expect("the plan");
        assert_eq!(27 - scroll - 5, 11);

        // Row 0 is the reading anchor, which this helper replaced with the same arithmetic.
        assert_eq!(scroll_to(&blocks, &collapsed, "a", 3, 0, 5), Some(19));
        assert_eq!(scroll_to(&blocks, &collapsed, "a", 50, 0, 5), Some(13));
        assert_eq!(scroll_to(&blocks, &collapsed, "c", 2, 0, 5), Some(0));
        assert_eq!(scroll_to(&blocks, &collapsed, "gone", 0, 0, 5), None);
    }

    #[test]
    fn the_transcript_hint_offers_p_only_with_a_long_plan_in_view() {
        assert_eq!(
            transcript_keys(false, None),
            "↑↓/PgUp scroll · G bottom · click a row of calls · t open all · Esc sidebar"
        );
        assert_eq!(
            transcript_keys(true, None),
            "↑↓/PgUp scroll · G bottom · click a row of calls · t close them · Esc sidebar"
        );
        assert!(transcript_keys(false, Some(false)).contains("G bottom · p expand plan · click"));
        assert!(transcript_keys(true, Some(true)).contains("G bottom · p collapse plan · click"));
    }

    #[test]
    fn a_checklist_keeps_its_plain_plan_rows() {
        let theme = Theme::new(Depth::TrueColor);
        let context = RenderContext {
            theme: &theme,
            text: Styles::new(&theme, theme.text()),
            bubble: Styles::new(&theme, theme.text()),
            reasoning: Styles::new(&theme, Style::new().fg(theme.muted)).dimmed(),
        };
        let todo = block(
            BlockKind::Plan,
            "todo_list",
            "Plan",
            "[x] Read the log\n[ ] Patch it",
        );
        let rows = text(&render_block(&todo, 40, &context, &Extra::None));
        assert_eq!(rows[1], "Plan");
        assert_eq!(rows[2].trim_end(), "  [x] Read the log");
        assert_eq!(rows[3].trim_end(), "  [ ] Patch it");
        let all = rows.concat();
        assert!(!all.contains('╭') && !all.contains("Expand"));
    }

    #[test]
    fn a_compaction_marker_shows_its_detail_and_follows_its_item() {
        let theme = Theme::new(Depth::TrueColor);
        let context = RenderContext {
            theme: &theme,
            text: Styles::new(&theme, theme.text()),
            bubble: Styles::new(&theme, theme.text()),
            reasoning: Styles::new(&theme, Style::new().fg(theme.muted)).dimmed(),
        };
        // A compaction starts with the count it compacts from and no title, then T3 sends the
        // same item finished.
        let running = json!({
            "id": "compaction-1",
            "type": "compaction",
            "ordinal": 1,
            "status": "running",
            "title": null,
            "driver": null,
            "summary": "Kept the plan 日本語 👩\u{200d}💻",
            "beforeTokenCount": 899_000,
            "updatedAt": "2026-10-08T10:00:00.000Z",
        });
        let mut completed = running.clone();
        completed["status"] = json!("completed");
        completed["afterTokenCount"] = json!(19_000);
        completed["updatedAt"] = json!("2026-10-08T10:00:05.000Z");
        let mut state = ThreadState::from_snapshot(&json!({
            "snapshotSequence": 1,
            "projection": {"thread": {"id": "t"}, "turnItems": [running]},
        }))
        .expect("a snapshot");

        let first = transcript::blocks(&state);
        assert_eq!(first.len(), 1);
        let lines = render_block(&first[0], 40, &context, &Extra::None);
        assert_eq!(
            text(&lines),
            [
                String::new(),
                format!("{} Compacting context {}", "─".repeat(10), "─".repeat(10)),
                "  899K → ? tokens".to_string(),
                "  Kept the plan 日本語 👩\u{200d}💻".to_string(),
                String::new(),
            ]
        );
        assert_eq!(lines[1].spans[1].style.fg, Some(theme.info));

        // The update replaces the row rather than adding one, with a new header and status.
        let update = json!({"kind": "event", "sequence": 2, "event": {
            "type": "turn-item.updated",
            "payload": completed,
        }});
        assert_eq!(
            state.apply(&update),
            Applied::Event("turn-item.updated".into())
        );
        let next = transcript::blocks(&state);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].item_id, first[0].item_id);
        assert_ne!(next[0].header, first[0].header);
        assert_ne!(next[0].status, first[0].status);
        let lines = render_block(&next[0], 60, &context, &Extra::None);
        assert_eq!(
            text(&lines),
            [
                String::new(),
                format!(
                    "{} Context compacted 899K → 19K tokens {}",
                    "─".repeat(11),
                    "─".repeat(12)
                ),
                "  Kept the plan 日本語 👩\u{200d}💻".to_string(),
                String::new(),
            ]
        );
        assert_eq!(lines[1].spans[1].style.fg, Some(theme.muted));
        // A reconnect replays the event, which changes nothing.
        assert_eq!(state.apply(&update), Applied::Duplicate);
        assert_eq!(transcript::blocks(&state).len(), 1);

        // The transcript is never drawn narrower than 10 columns. There the label is cut, the
        // detail wraps under it and the joined emoji stays whole on its own row.
        let rows = text(&render_block(&first[0], 10, &context, &Extra::None));
        assert_eq!(
            rows,
            [
                "",
                "── Com… ──",
                "  899K →",
                "  ?",
                "  tokens",
                "  Kept",
                "  the",
                "  plan",
                "  日本語",
                "  👩\u{200d}💻",
                "",
            ]
        );
        for width in [10, 11, 13] {
            for item in [&first[0], &next[0]] {
                let rows = text(&render_block(item, width, &context, &Extra::None));
                assert!(rows.iter().all(|row| row.width() <= width), "{rows:?}");
            }
        }

        // Other notices keep their bare rule, even while running.
        let mut notice = block(BlockKind::Notice, "system_notice", "Model changed", "");
        notice.status = "running".into();
        let lines = render_block(&notice, 40, &context, &Extra::None);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].spans[1].style.fg, Some(theme.muted));
    }

    #[test]
    fn a_compaction_is_prepared_once_for_each_change_to_its_item() {
        fn context(theme: &Theme) -> RenderContext<'_> {
            RenderContext {
                theme,
                text: Styles::new(theme, theme.text()),
                bubble: Styles::new(theme, Style::new().fg(theme.fg).bg(theme.bubble)),
                reasoning: Styles::new(theme, Style::new().fg(theme.muted)).dimmed(),
            }
        }
        // Each block the next frame draws from: its item, the number it was made under and
        // where its text is kept, which moves only when the block is made again.
        fn frame(open: &mut OpenThread) -> Vec<(String, u64, *const u8)> {
            open.prepare();
            let prepared = &open.prepared;
            prepared
                .blocks
                .iter()
                .zip(&prepared.versions)
                .map(|(block, version)| (block.item_id.clone(), *version, block.body.as_ptr()))
                .collect()
        }
        fn ids(blocks: Vec<(String, u64, *const u8)>) -> Vec<String> {
            blocks.into_iter().map(|(id, ..)| id).collect()
        }
        let compaction = |id: &str, ordinal: u64, status: &str| {
            json!({
                "id": id,
                "type": "compaction",
                "ordinal": ordinal,
                "status": status,
                "title": null,
                "driver": null,
                "summary": format!("Kept\u{1b}[2J the plan for {id} 日本語"),
                "beforeTokenCount": 899_000,
                "updatedAt": "2026-10-08T10:00:00.000Z",
            })
        };
        let snapshot = |sequence: u64, items: Value| {
            json!({"kind": "snapshot", "snapshotSequence": sequence, "projection": {
                "thread": {"id": "t"},
                "turnItems": items,
            }})
        };
        let event = |sequence: u64, kind: &str, payload: &Value| {
            json!({"kind": "event", "sequence": sequence, "event": {
                "type": kind,
                "payload": payload,
            }})
        };

        // A finished compaction, a running one and an answer, as the watch of a thread just
        // opened delivers them. The first frame makes a block for each.
        let mut done = compaction("c1", 1, "completed");
        done["afterTokenCount"] = json!(19_000);
        let running = compaction("c2", 2, "running");
        let answer = json!({"id": "a1", "type": "assistant_message", "ordinal": 3,
            "text": "Done"});
        let mut open = OpenThread::new("t".into(), mpsc::unbounded_channel().1);
        let items = json!([done, running, answer]);
        assert_eq!(open.apply(&snapshot(1, items)), Applied::Snapshot);
        let first = frame(&mut open);
        assert_eq!(open.prepared.made, 3);
        let (c1, c2) = (&open.prepared.blocks[0], &open.prepared.blocks[1]);
        assert_eq!(c1.header, "Context compacted 899K → 19K tokens");
        assert_eq!(c1.body, "Kept[2J the plan for c1 日本語");
        assert_eq!(c2.header, "Compacting context");
        assert_eq!(c2.body, "899K → ? tokens\nKept[2J the plan for c2 日本語");

        // A frame drawn for a key, the clock or a resize follows no event. At any width and in
        // either palette it draws from the same blocks, so no summary is cleaned and no count
        // written again.
        for theme in [Theme::new(Depth::TrueColor), Theme::new(Depth::Indexed)] {
            for width in [10, 40, 120] {
                for block in &open.prepared.blocks {
                    let rows = text(&render_block(block, width, &context(&theme), &Extra::None));
                    assert!(rows.iter().all(|row| row.width() <= width), "{rows:?}");
                }
                assert_eq!(frame(&mut open), first);
            }
        }

        // A replayed event, a run's update and the end of the replay change no item.
        assert_eq!(
            open.apply(&event(1, "turn-item.updated", &done)),
            Applied::Duplicate
        );
        let run = json!({"id": "r1", "status": "running", "ordinal": 1});
        assert_eq!(
            open.apply(&event(2, "run.updated", &run)),
            Applied::Event("run.updated".into())
        );
        assert_eq!(
            open.apply(&json!({"kind": "synchronized"})),
            Applied::Synchronized
        );
        assert_eq!(frame(&mut open), first);
        assert_eq!(open.prepared.made, 3);

        // The running compaction finishes. T3 sends the item whole, here under the same
        // `updatedAt`, and the next frame makes its block again and keeps the others.
        let mut finished = running;
        finished["status"] = json!("completed");
        finished["afterTokenCount"] = json!(1_500);
        open.apply(&event(3, "turn-item.updated", &finished));
        let second = frame(&mut open);
        assert_eq!(open.prepared.made, 4);
        assert_eq!((&second[0], &second[2]), (&first[0], &first[2]));
        assert_ne!(second[1].1, first[1].1);
        let c2 = &open.prepared.blocks[1];
        assert_eq!(c2.header, "Context compacted 899K → 1.50K tokens");
        assert_eq!(c2.body, "Kept[2J the plan for c2 日本語");

        // A new summary alone makes the block again too.
        let mut summarized = finished;
        summarized["summary"] = json!("Kept only the plan");
        open.apply(&event(4, "turn-item.updated", &summarized));
        let third = frame(&mut open);
        assert_eq!(open.prepared.made, 5);
        assert_eq!((&third[0], &third[2]), (&first[0], &first[2]));
        assert_ne!(third[1].1, second[1].1);
        assert_eq!(open.prepared.blocks[1].body, "Kept only the plan");

        // Updates that arrive between two frames make the block once, at the next frame.
        for (sequence, count) in [(5, 1_600), (6, 1_700), (7, 1_800)] {
            let mut next = summarized.clone();
            next["afterTokenCount"] = json!(count);
            open.apply(&event(sequence, "turn-item.updated", &next));
        }
        open.prepare();
        assert_eq!(open.prepared.made, 6);
        let c2 = &open.prepared.blocks[1];
        assert_eq!(c2.header, "Context compacted 899K → 1.80K tokens");
        assert!(open.prepared.changed.is_empty());

        // A reconnect's snapshot no longer has the first compaction. Its block is dropped, and
        // the others are made again, since a snapshot can change any item.
        let items = json!([summarized, answer]);
        assert_eq!(open.apply(&snapshot(9, items)), Applied::Snapshot);
        assert_eq!(ids(frame(&mut open)), ["c2", "a1"]);
        assert_eq!(open.prepared.made, 8);

        // Opening another thread replaces the open one, as `open_selected` does, and the blocks
        // go with it. The new thread starts with none and makes only its own.
        open = OpenThread::new("u".into(), mpsc::unbounded_channel().1);
        assert!(frame(&mut open).is_empty());
        open.apply(&snapshot(1, json!([compaction("c9", 1, "failed")])));
        assert_eq!(ids(frame(&mut open)), ["c9"]);
        assert_eq!(open.prepared.made, 1);
    }
}
