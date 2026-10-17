//! The interactive client. It draws only after input or a server event, at most 30 times a
//! second. Hiding, showing or resizing the sidebar draws at once, so a click that follows lands
//! on the new layout. Two timers can wake it besides: a one-second tick, armed while the open
//! thread has a run going, its worktree setup card on screen shows a setup still running, or a
//! sidebar card on screen reads Working or Goal, so the elapsed-time labels and spinners
//! advance, and a one-shot timer for the moment the soonest snooze ends, which no server event
//! marks. An idle TUI uses no CPU.

mod appearance;
mod banner;
mod composer;
mod details;
mod environment;
mod label;
mod lineage;
mod markdown;
mod palettes;
mod picker;
mod plan;
mod provider;
mod pull_requests;
mod scroll;
mod setup;
mod sidebar;
mod tasks;
mod theme;
mod unsent;
mod wide_emoji;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Stdout;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use crate::client::{Client, IfBusy, WatchEvent};
use crate::models::Choice;
use crate::projection::{Applied, ShellState, ThreadState, apply_config, is_active_status, status};
use crate::pull_request::{self, PullRequestError, Reference};
use crate::settings::{AppearanceMode, Settings};
use crate::transcript::{self, BlockKind, ChangeLine};
use composer::Composer;
use details::Details;
use environment::{Feed, Identity};
use markdown::Styles;
use picker::{Item, Kind, Pick};
use pull_requests::Part;
use scroll::{Pin, Scroll};
use sidebar::{Badges, Capabilities, Shelf, ShelfChoices, Sidebar, View};
use theme::{
    Appearance, Depth, Theme, duration_label, model_display_name, now_ms, parse_iso_ms,
    runtime_mode_label,
};
use unsent::{Failed, Unsent};

const FRAME: Duration = Duration::from_millis(33);
/// While a run is active, elapsed-time labels advance once a second.
const TICK: Duration = Duration::from_secs(1);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Text rows the composer grows to before it scrolls.
const COMPOSER_MAX_ROWS: usize = 6;
/// The button that hides and shows the sidebar, after the panel icon on the desktop's.
const TOGGLE: &str = " ◧ ";
/// The narrowest width the sidebar can be given, in columns. The desktop's is 13rem against a
/// 16rem default. Twenty columns keep the wordmark and a card's first words.
const NARROWEST_SIDEBAR: u16 = 20;
/// The columns the sidebar leaves the conversation on a screen at least this wide, as the
/// layout did before the sidebar could be resized. A narrower screen gives the sidebar no
/// columns and the conversation all of them. The desktop keeps 40rem, which would be most of
/// an 80-column terminal.
const NARROWEST_MAIN: u16 = 20;
/// How far `[` and `]` move the sidebar's edge, in columns.
const SIDEBAR_STEP: u16 = 2;
/// The fewest rows a screen has for the sidebar and the main column to keep a blank row at the
/// top, over the wordmark and the breadcrumb.
const ROOMY: u16 = 20;
/// The columns of the thread's title the breadcrumb keeps before it gives any to the
/// environment, while the sidebar doesn't show the environment.
const KEPT_TITLE: usize = 16;

/// Opens T3's worktree setup subscription for a thread: `Client::watch_worktree_setup`, or a
/// test's stand-in.
type WatchSetup = Box<dyn Fn(&str) -> mpsc::UnboundedReceiver<WatchEvent>>;
/// Reads a pull request from its host in the background, and sends the answer as an action
/// with the ticket it was given.
type ReadPull = Box<dyn Fn(Reference, u64)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Transcript,
    Composer,
    /// The thread's details: a dialog of them, which always has the keyboard while it shows,
    /// or their pane, after a click on it or Tab.
    Details,
}

/// A drag of the sidebar's right edge, from the left button going down on it until it comes
/// up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Drag {
    /// The sidebar's width at the press, which comes back when the drag ends without a release.
    from: u16,
    /// The width the pointer has dragged the sidebar to.
    width: u16,
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
/// its counts written once each time its item changes. A handoff that reads its models from
/// the runs is described again when its item changes, and when a run it could read is added or
/// changes its ordinal, provider or model. A run's status and times don't count.
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
    /// Handoffs whose endpoints read the thread's runs, because T3 didn't stamp models on both
    /// ends. While it's empty, `OpenThread::apply` doesn't look at what a run event changed.
    run_readers: HashSet<String>,
    /// How many blocks are prompts, and where the first is, which place and retire the
    /// worktree setup card. They are counted as the blocks are put in order, so a frame
    /// doesn't look through the blocks for them.
    users: usize,
    first_user: Option<usize>,
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

    /// Marks the handoffs that read the runs, after an event changed what they read of one.
    fn mark_run_readers(&mut self) {
        self.changed.extend(self.run_readers.iter().cloned());
    }

    /// Brings the blocks up to date with `state`. The block of an item nothing has marked moves
    /// over as it was, and the blocks of items that have left the thread are dropped. An item
    /// with no row is described again, which stops at its type, its title or its blank text.
    fn refresh(&mut self, state: &ThreadState) {
        if self.current && self.changed.is_empty() {
            return;
        }
        let current = self.current;
        if !current {
            self.run_readers.clear();
        }
        self.users = 0;
        self.first_user = None;
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
                None => {
                    if transcript::reads_runs(item) {
                        self.run_readers.insert(id.to_string());
                    } else {
                        self.run_readers.remove(id);
                    }
                    match transcript::describe(item, state.runs_for(item)) {
                        Some(block) => {
                            self.made += 1;
                            (block, self.made)
                        }
                        None => continue,
                    }
                }
            };
            if block.kind == BlockKind::User {
                self.first_user.get_or_insert(self.blocks.len());
                self.users += 1;
            }
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
    /// T3's configuration from `server.getConfig`, with the count of changes from the config
    /// subscription when the read was asked for.
    Config {
        config: Value,
        revision: u64,
    },
    /// What one tool printed, for the row that asked. `None` when T3 couldn't be reached.
    Output {
        item_id: String,
        revision: String,
        text: Option<String>,
    },
    Sent {
        note: String,
    },
    /// What a link's host said of its pull request, or why the read failed, for the read with
    /// this ticket.
    PullRequest {
        ticket: u64,
        part: Part,
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
    /// The system's appearance, read when the color scheme turned to System, from the read
    /// numbered `read`.
    SystemAppearance {
        appearance: Appearance,
        read: u64,
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
    /// The thread's error from T3, for the banner.
    error: banner::Derived,
    /// The thread's worktree setup, its subscription and its card.
    setup: setup::Watch,
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
            error: banner::Derived::default(),
            setup: setup::Watch::default(),
        }
    }

    /// Applies one item from the thread's watch, and notes what it changed for the next frame.
    fn apply(&mut self, item: &Value) -> Applied {
        let state = self.state.get_or_insert_with(ThreadState::default);
        let id = item.pointer("/event/payload/id").and_then(Value::as_str);
        // Whether a block holds a handoff that reads the runs. With none, a run event has no
        // block to mark, so the run isn't looked up on either side of it. A handoff whose item
        // arrives or changes after a frame is described by the next one anyway, from the runs
        // as they are then.
        let readers = !self.prepared.run_readers.is_empty();
        // What a handoff reads of the run a run event changes, from before the event.
        let before = item
            .pointer("/event/type")
            .and_then(Value::as_str)
            .filter(|kind| readers && kind.starts_with("run."))
            .and(id)
            .and_then(|run_id| state.run(run_id))
            .map(transcript::handoff_fields);
        let applied = state.apply(item);
        self.error.note(&applied, item);
        match &applied {
            Applied::Synchronized => self.connection = "live".into(),
            Applied::Snapshot => self.prepared.mark(None),
            Applied::Event(kind) if kind.starts_with("run.") => {
                if let Some(run_id) = id {
                    self.run_changes.insert(run_id.to_string(), state.sequence);
                    if readers && state.run(run_id).map(transcript::handoff_fields) != before {
                        self.prepared.mark_run_readers();
                    }
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

    /// Lays the worktree setup card out for a frame `width` columns wide, after `prepare`. The
    /// prompts were counted as the blocks were made, and whether the turn started was noted
    /// when an event last changed the runs, so this reads neither the items nor the runs.
    fn lay_out_setup(&mut self, width: usize, now: i64, theme: &Theme) {
        let turn = setup::Turn {
            started: self.setup.turn_started,
            follow_up: self.prepared.users > 1,
        };
        self.setup.lay_out(turn, width, now, theme);
    }
}

struct App {
    client: Arc<Client>,
    theme: Theme,
    /// The color scheme, which says which half of the theme draws. Saved between runs.
    appearance_mode: AppearanceMode,
    /// The system's appearance as last read, for the System scheme. `None` before a read has
    /// answered.
    system_appearance: Option<Appearance>,
    /// How many reads of the system's appearance have started since the one at start, which
    /// numbers each.
    system_reads: u64,
    /// The number of the read `system_appearance` came from. Reads can answer out of order,
    /// and an answer from an older read is dropped.
    system_read_kept: u64,
    /// T3's configuration, with its providers and models. The config subscription keeps it
    /// current, and it stays as it was while the subscription reconnects.
    config: Option<Value>,
    /// False once T3 refuses the config subscription. Each menu then reads the configuration
    /// with `server.getConfig`, as t3term did before it subscribed.
    config_subscribed: bool,
    /// How many changes the config subscription has made, so a `server.getConfig` answer asked
    /// for before the latest of them can't put older providers back.
    config_revision: u64,
    /// Model, effort and mode choices per thread, sent with the thread's next message.
    drafts: HashMap<String, Choice>,
    /// Messages that failed to send, until they go out or reach T3 late.
    unsent: Unsent,
    picker: Option<Picker>,
    shell: Option<ShellState>,
    /// The badge of each project in `shell`, which the sidebar's cards and the breadcrumb show.
    badges: Badges,
    /// Where the sidebar's thread feed stands, which the sidebar's dot shows. The open
    /// thread's watch keeps its own state.
    feed: Feed,
    /// The environment's name and address from discovery, cleaned once, for the sidebar's
    /// header, or the end of the breadcrumb row while the sidebar doesn't show it.
    environment: Identity,
    sidebar: Sidebar,
    /// Whether the sidebar is hidden, which gives the main column its width. Saved between
    /// runs. Focus never rests on a hidden sidebar.
    sidebar_hidden: bool,
    /// The sidebar's width as a drag of its edge or `[` and `]` left it, or `None` for the
    /// width t3term picks from the screen's. Saved between runs. A screen too narrow for it
    /// draws the sidebar narrower and leaves it as it is.
    sidebar_width: Option<u16>,
    /// A drag of the sidebar's edge under way.
    drag: Option<Drag>,
    /// Set when the sidebar is hidden, shown or resized, or one of its shelves opens or
    /// closes, so the next frame draws at once instead of waiting out the frame budget. Until
    /// it does, clicks would land on the old layout.
    layout_changed: bool,
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
    /// The provider status banner over the top of the conversation, for the provider instance
    /// the composer would send with, and the notice dismissed in this window.
    provider_banner: provider::Banner,
    /// The thread error banner under it, with the errors dismissed this session and the sends
    /// that failed.
    banner: banner::Banner,
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
    /// The button at the top left of the conversation that hides and shows the sidebar.
    sidebar_toggle: Rect,
    /// The open thread's details, which only this window opens and closes.
    details: Details,
    /// The whole screen the last frame drew, which a drag and `[` and `]` work out the
    /// sidebar's width against.
    screen: Rect,
    /// Opens the open thread's worktree setup subscription.
    watch_setup: WatchSetup,
    /// Reads the pull request whose details opened.
    read_pull: ReadPull,
    /// Set once T3 refuses the worktree setup subscription. T3 refuses it for the session or
    /// the server, never for one thread, so no thread asks again.
    setup_refused: bool,
    quit: bool,
}

pub async fn run(client: Arc<Client>) -> Result<()> {
    let mut screen = Screen(Some(init()));
    crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let Some(terminal) = screen.0.as_mut() else {
        return Ok(());
    };
    event_loop(terminal, client).await
}

/// The terminal the TUI draws on: ratatui's crossterm backend on stdout, through
/// [`WideEmoji`](wide_emoji::WideEmoji).
type Term = Terminal<wide_emoji::WideEmoji<CrosstermBackend<Stdout>>>;

/// `ratatui::init` with the backend `Term` names: a panic hook that puts the terminal back,
/// raw mode and the alternate screen. Like it, it panics when the terminal can't be set up.
fn init() -> Term {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        hook(info);
    }));
    let set_up = || -> std::io::Result<Term> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
        Terminal::new(wide_emoji::WideEmoji(CrosstermBackend::new(
            std::io::stdout(),
        )))
    };
    set_up().expect("failed to initialize terminal")
}

/// Owns the terminal while the TUI runs and puts it back when dropped, including when main
/// drops the TUI after a signal.
struct Screen(Option<Term>);

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

/// Reads the pull request `reference` names from its host through T3: its detail and its
/// conversation at once, as the desktop's panel asks for them. Gives `send` each answer as it
/// comes, or why it failed, and then `Part::Done`. A sentence of T3's own, such as a host the
/// server has no credentials for, reads as T3 wrote it. Any other failure of the detail says
/// that the read failed, and the Comments section says that of the conversation.
async fn read_pull_request(client: &Client, reference: &Reference, send: impl Fn(Part)) {
    fn failed(e: &dyn std::fmt::Display) -> String {
        format!("Couldn't read this pull request: {e}")
    }
    match client.rpc().await {
        Ok(rpc) => {
            let detail = async {
                let answer = match pull_request::detail(&rpc, reference).await {
                    Ok(detail) => Ok(Box::new(detail)),
                    Err(e @ PullRequestError::Rpc(_)) => Err(failed(&e)),
                    Err(e) => Err(e.to_string()),
                };
                send(Part::Detail(answer));
            };
            let activity = async {
                let answer = pull_request::activity(&rpc, reference).await;
                send(Part::Activity(
                    answer.map(Box::new).map_err(|e| e.to_string()),
                ));
            };
            tokio::join!(detail, activity);
        }
        Err(e) => send(Part::Detail(Err(failed(&e)))),
    }
    send(Part::Done);
}

async fn event_loop(terminal: &mut Term, client: Arc<Client>) -> Result<()> {
    let (actions, mut action_results) = mpsc::unbounded_channel();
    let mut shell_events = client.watch_shell(None);
    // Dropped when the loop ends, which ends the subscription.
    let mut config_events = client.watch_config();
    let settings = Settings::load().await;
    // Read before the first frame, so System on a light Mac draws no dark frame first.
    let system = match settings.appearance_mode {
        AppearanceMode::System => appearance::system().await,
        _ => None,
    };
    let shown = appearance::resolve(settings.appearance_mode, system, Appearance::Dark);
    let mut app = App::new(
        client,
        Theme::of(settings.theme, shown, Depth::detect()),
        &settings,
        actions,
    );
    app.system_appearance = system;
    let mut input = EventStream::new();
    let mut dirty = true;
    let mut last_draw = Instant::now() - FRAME;
    let mut tick_at = Instant::now() + TICK;

    while !app.quit {
        if dirty && (app.layout_changed || last_draw.elapsed() >= FRAME) {
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
        // The open thread's watch and its worktree setup subscription, which are its own.
        let (thread_events, setup_events) = match app.open.as_mut() {
            Some(open) => (Some(&mut open.events), open.setup.events.as_mut()),
            None => (None, None),
        };
        let thread_event = async move {
            match thread_events {
                Some(events) => events.recv().await,
                None => std::future::pending().await,
            }
        };
        let setup_event = async move {
            match setup_events {
                Some(events) => events.recv().await,
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
            Some(event) = config_events.recv() => {
                app.on_config_event(event);
                dirty = true;
            }
            Some(event) = thread_event => {
                app.on_thread_event(event);
                dirty = true;
            }
            // `None` once the subscription has ended, which closes it for good.
            event = setup_event => dirty |= app.on_setup_event(event),
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
/// the last frame drew its worktree setup card while its subscription says the setup runs, or
/// the last frame drew a sidebar card that reads Working or Goal. Work the sidebar doesn't
/// show, because it is behind a closed shelf, scrolled away or in a hidden sidebar, doesn't
/// wake the TUI, and neither does a setup card out of view.
fn needs_tick(open: Option<&OpenThread>, sidebar: &Sidebar) -> bool {
    // A closed watch never hears that its run finished, so its last state can't count.
    let open_run = open
        .filter(|o| o.connection != "closed")
        .and_then(|o| o.state.as_ref())
        .is_some_and(|s| s.active_run().is_some());
    let setup = open.is_some_and(|o| o.setup.ticks());
    open_run || setup || sidebar.drew_working()
}

/// The sidebar's column, `sidebar_width` columns wide, and the main column, which takes the
/// rest. A hidden sidebar's column has no width, and the main column takes the whole screen.
fn columns(area: Rect, sidebar_width: u16) -> (Rect, Rect) {
    let sidebar_width = sidebar_width.min(area.width);
    let sidebar = Rect {
        width: sidebar_width,
        ..area
    };
    let main = Rect {
        x: area.x + sidebar_width,
        width: area.width - sidebar_width,
        ..area
    };
    (sidebar, main)
}

/// The widest the sidebar gets on a screen `width` columns wide: half of it, and less when that
/// would leave the conversation fewer than `NARROWEST_MAIN` columns. On a screen narrower than
/// `NARROWEST_MAIN`, that is no columns.
fn widest_sidebar(width: u16) -> u16 {
    (width / 2).min(width.saturating_sub(NARROWEST_MAIN))
}

/// How wide the sidebar draws on a screen `width` columns wide. With no width chosen it takes a
/// quarter of the screen, from 26 to 34 columns. A chosen width draws as chosen when the screen
/// has room for it and as wide as the screen allows when it hasn't, so it comes back when the
/// screen grows. A screen too narrow for `NARROWEST_SIDEBAR` gets the widest it allows either
/// way.
fn sidebar_width(width: u16, chosen: Option<u16>) -> u16 {
    let wanted = match chosen {
        Some(chosen) => chosen.max(NARROWEST_SIDEBAR),
        None => (width / 4).clamp(26, 34),
    };
    wanted.min(widest_sidebar(width))
}

/// The pane Tab moves focus to from `focus`, or BackTab when `forward` is false. A hidden
/// sidebar is left out of the cycle, and the thread's details are in it while `details` says
/// they show as a pane.
fn cycle(focus: Focus, forward: bool, sidebar_hidden: bool, details: bool) -> Focus {
    let panes: Vec<Focus> = [
        Focus::Sidebar,
        Focus::Composer,
        Focus::Transcript,
        Focus::Details,
    ]
    .into_iter()
    .filter(|pane| match pane {
        Focus::Sidebar => !sidebar_hidden,
        Focus::Details => details,
        Focus::Composer | Focus::Transcript => true,
    })
    .collect();
    let at = panes.iter().position(|&pane| pane == focus).unwrap_or(0);
    let step = if forward { 1 } else { panes.len() - 1 };
    panes[(at + step) % panes.len()]
}

/// The spinner frame for a wall-clock time, so every spinner on screen turns together.
fn spinner_frame(now: i64) -> &'static str {
    SPINNER[(now / 1000).rem_euclid(SPINNER.len() as i64) as usize]
}

/// The main column `main` less a blank column on each side, like the GUI's padding.
fn padded(main: Rect) -> Rect {
    Rect {
        x: main.x + 1,
        width: main.width.saturating_sub(2),
        ..main
    }
}

/// The blank rows over the wordmark and the breadcrumb on a screen `height` rows tall: one
/// from `ROOMY` rows, and none on a shorter screen, which needs every row.
fn top_inset(height: u16) -> u16 {
    u16::from(height >= ROOMY)
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
    /// The TUI before anything has arrived from T3, laid out as the settings file says. Focus
    /// starts on the sidebar, or on the composer when the sidebar starts hidden.
    fn new(
        client: Arc<Client>,
        theme: Theme,
        settings: &Settings,
        actions: mpsc::UnboundedSender<ActionResult>,
    ) -> App {
        let watcher = client.clone();
        let (reader, answers) = (client.clone(), actions.clone());
        let environment = Identity::new(client.runtime.label.as_deref(), &client.runtime.origin);
        App {
            client,
            theme,
            appearance_mode: settings.appearance_mode,
            system_appearance: None,
            system_reads: 0,
            system_read_kept: 0,
            config: None,
            config_subscribed: true,
            config_revision: 0,
            drafts: HashMap::new(),
            unsent: Unsent::default(),
            picker: None,
            shell: None,
            badges: Badges::default(),
            feed: Feed::Connecting,
            environment,
            sidebar: Sidebar::with_choices(ShelfChoices {
                working_enabled: settings.sidebar_working_shelf_enabled,
                working_open: settings.sidebar_working_shelf_expanded,
                done_open: settings.sidebar_done_shelf_expanded,
                settled_open: settings.sidebar_settled_shelf_expanded,
            }),
            sidebar_hidden: settings.sidebar_hidden,
            sidebar_width: settings.sidebar_width,
            drag: None,
            layout_changed: false,
            open: None,
            focus: if settings.sidebar_hidden {
                Focus::Composer
            } else {
                Focus::Sidebar
            },
            composer: Composer::default(),
            scroll: Scroll::default(),
            open_bundles: HashSet::new(),
            bundle_rows: Vec::new(),
            expanded_plans: HashSet::new(),
            drawn_plans: Vec::new(),
            tasks: tasks::Drawer::default(),
            provider_banner: provider::Banner::default(),
            banner: banner::Banner::default(),
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
            sidebar_toggle: Rect::default(),
            details: Details::default(),
            screen: Rect::default(),
            watch_setup: Box::new(move |thread_id: &str| watcher.watch_worktree_setup(thread_id)),
            read_pull: Box::new(move |reference, ticket| {
                let (client, answers) = (reader.clone(), answers.clone());
                tokio::spawn(async move {
                    let send = |part| {
                        let _ = answers.send(ActionResult::PullRequest { ticket, part });
                    };
                    read_pull_request(&client, &reference, send).await;
                });
            }),
            setup_refused: false,
            quit: false,
        }
    }

    // ---- server events ----

    fn on_shell_event(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Item(item) => {
                let shell = self.shell.get_or_insert_with(ShellState::default);
                let applied = shell.apply(&item);
                // A project's badge is worked out again only when the project changes.
                self.badges.note(&applied, &item, shell);
                // The details show the shell's copy of the open thread until its watch sends
                // the thread, and its project from the shell's.
                let open_id = self.open.as_ref().map(|open| open.id.as_str());
                self.details.note_shell(&applied, &item, open_id);
                if item["kind"] == "synchronized" {
                    self.feed = Feed::Live;
                }
                self.rebuild_rows();
                self.settle_draft();
                // The shell's copy of the open thread names its model and provider instance.
                self.follow_provider(false);
                // It has the thread's worktree path too.
                self.follow_setup();
            }
            // The sidebar keeps the rows it has until the new subscription catches up.
            WatchEvent::Reconnecting { .. } => self.feed = Feed::Reconnecting,
            // The watcher stopped, so the rows stay as they were until t3term restarts. The
            // message goes with the next key, and the dot stays.
            WatchEvent::Failed(message) => {
                self.feed = Feed::Failed;
                // Projects the details wait for won't come now.
                self.details.mark();
                self.message = Some((message, true));
            }
        }
    }

    /// Applies an item from T3's config subscription. A reconnect keeps the configuration as it
    /// was until the new subscription's snapshot replaces it.
    fn on_config_event(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Item(item) => {
                if self.update_config(|config| apply_config(config, &item) != Applied::Ignored) {
                    self.config_revision += 1;
                }
            }
            WatchEvent::Reconnecting { .. } => {}
            // Such as a server without the subscription. Read the configuration once now, and
            // again each time a menu opens.
            WatchEvent::Failed(_) => {
                self.config_subscribed = false;
                self.load_config();
            }
        }
    }

    fn on_thread_event(&mut self, event: WatchEvent) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        match event {
            WatchEvent::Item(item) => {
                let applied = open.apply(&item);
                self.details.note_thread(&applied);
                // A fresh snapshot means the watch reconnected, so output T3 couldn't send
                // before is worth asking for again.
                if applied == Applied::Snapshot {
                    self.cache.clear();
                    self.outputs.retry_failed();
                }
                // The projection's thread names its model and provider instance too, which the
                // composer goes by when the shell doesn't list the thread. Other events, such
                // as each piece of a streamed answer, leave the provider banner alone.
                let thread_changed = match &applied {
                    Applied::Snapshot => true,
                    Applied::Event(kind) => kind.starts_with("thread."),
                    _ => false,
                };
                if self.settle_draft() || thread_changed {
                    self.follow_provider(false);
                }
                // The thread's worktree path, and whether its run is preparing a worktree or
                // has started its turn, change only with the thread or its runs.
                let runs_changed =
                    matches!(&applied, Applied::Event(kind) if kind.starts_with("run."));
                if thread_changed || runs_changed {
                    self.follow_setup();
                }
                self.drop_landed();
            }
            WatchEvent::Reconnecting { reason, .. } => {
                open.connection = format!("reconnecting: {reason}")
            }
            // The thread is closed to t3term, so its worktree setup is too.
            WatchEvent::Failed(message) => {
                open.connection = "closed".into();
                open.setup.close();
                // The details go by the shell's copy of the thread from now on.
                self.details.mark();
                self.message = Some((message, true));
            }
        }
        self.follow_tasks();
        self.follow_banner();
    }

    /// Opens or closes the open thread's worktree setup subscription as the desktop's chat view
    /// does (`worktreeSetupQuery` in ChatView.tsx): open while the thread has a worktree or its
    /// activity run is preparing one, closed otherwise. It notes whether that run has started
    /// its turn, for the card. It runs when a thread opens and after what can change these: a
    /// snapshot, an event for the thread or a run, and the shell's copy of the thread. So
    /// neither a frame, a key nor a piece of a streamed answer looks. A subscription that ended
    /// for good isn't asked for again, nor is one T3 refused, nor one for a thread whose watch
    /// closed.
    fn follow_setup(&mut self) {
        let has_worktree = self
            .open_thread_json()
            .is_some_and(|thread| thread.get("worktreePath").is_some_and(Value::is_string));
        let Some(open) = self.open.as_mut() else {
            return;
        };
        // The closed watch closed the setup subscription, and hears nothing more of the
        // thread's runs. So neither the runs as it last saw them nor the shell's copy of the
        // thread opens another. The thread subscribes again when it opens with a new watch.
        if open.connection == "closed" {
            return;
        }
        let run = open.state.as_ref().and_then(ThreadState::activity_run);
        let preparing = run.is_some_and(|run| status(run) == "preparing");
        open.setup.turn_started =
            run.is_some_and(|run| run.get("startedAt").is_some_and(|at| !at.is_null()));
        if !has_worktree && !preparing {
            open.setup.close();
        } else if !self.setup_refused && open.setup.wants() {
            open.setup.watch((self.watch_setup)(&open.id));
        }
    }

    /// Applies an item from the open thread's worktree setup subscription, or `None` once it
    /// ended, and says whether the card needs drawing again. A refusal says nothing: the card
    /// only reads, and the thread works without it.
    fn on_setup_event(&mut self, event: Option<WatchEvent>) -> bool {
        let Some(open) = self.open.as_mut() else {
            return false;
        };
        match open.setup.apply(event, &open.id) {
            setup::Outcome::Changed => true,
            setup::Outcome::Unchanged => false,
            setup::Outcome::Refused => {
                self.setup_refused = true;
                false
            }
        }
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

    /// Brings the error banner up to date with the open thread. It runs after each change to
    /// the thread or to its send errors, and after the reader dismisses one, which can show
    /// T3's own error from under a send error. It reads the error only after something that
    /// can change it: an event for a run, a provider session, the thread or an error item, a
    /// snapshot, a send error that came or went, or a dismissal. A thread just opened counts
    /// too, as its error hasn't been read. Each piece of a streamed answer, a frame, and a key
    /// that only opens or scrolls the banner leave the error unread, however long a provider
    /// session made it.
    fn follow_banner(&mut self) {
        match self.open.as_mut() {
            Some(open) if open.error.stale() || self.banner.stale() => {
                let runtime = open.error.get(open.state.as_ref());
                self.banner.follow(Some(&open.id), runtime);
            }
            Some(_) => {}
            None => self.banner.follow(None, None),
        }
    }

    /// Brings the provider banner up to date with the provider instance the composer would
    /// send the next message with, the one the model chip shows: the open thread's selection
    /// with its unsent choice applied. It works the selection out again only when T3's config
    /// changed, as `config_changed` says, or when the open thread, its model selection, its
    /// provider instance or its unsent choice did. Callers run it after the events that can
    /// change one of those, so neither a frame nor a streamed answer looks.
    fn follow_provider(&mut self, config_changed: bool) {
        let inputs = {
            let open = self.open.as_ref().and_then(|open| {
                let thread = self.open_thread_json()?;
                Some((open.id.as_str(), thread, self.drafts.get(&open.id)))
            });
            if !config_changed && !self.provider_banner.stale(open) {
                return;
            }
            open.map(|(id, thread, draft)| provider::Inputs::new(id, thread, draft))
        };
        // The provider goes by its instance id alone. One with no models left still has its
        // status shown, and no other provider stands in for one T3 no longer lists.
        let selection = self.settings_view().map(|view| view.selection);
        let instance = selection
            .as_ref()
            .and_then(|selection| selection["instanceId"].as_str());
        let provider = instance
            .zip(self.config.as_ref())
            .and_then(|(instance, config)| {
                crate::models::providers(config)
                    .iter()
                    .find(|provider| provider["instanceId"] == instance)
            });
        self.provider_banner.follow(inputs, provider);
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

    /// Whether the environment links pull requests to threads, as its capabilities'
    /// `threadPullRequests` says, from the same descriptor as `capabilities`.
    fn links_supported(&self) -> bool {
        self.environment_can("threadPullRequests")
    }

    /// Whether the environment's capabilities turn `capability` on, from the same descriptor
    /// as `capabilities`.
    fn environment_can(&self, capability: &str) -> bool {
        let capabilities = match self.config.as_ref() {
            Some(config) => &config["environment"]["capabilities"],
            None => &self.client.runtime.capabilities,
        };
        capabilities[capability] == Value::Bool(true)
    }

    /// Asks the host for the pull request of the link whose details just opened, through the
    /// open thread's project, as the desktop's panel reads a link
    /// (apps/web/src/components/pullRequest/ThreadPullRequestsPanel.tsx). Only an environment
    /// whose capabilities have `pullRequests` reads them. The details show the answer when it
    /// comes, and a reference T3's schema would refuse fails at once.
    fn read_opened_pull(&mut self) {
        let project = match self.details_record() {
            details::Record::Found(thread) => thread
                .get("projectId")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        };
        let enabled = project.is_some() && self.environment_can("pullRequests");
        // A read that waits goes once its details don't show.
        let shown = self.details.shows_pulls();
        self.details.pulls.shown(shown);
        if !shown {
            return;
        }
        // The last frame's time can be long past, and a test can move it on.
        let now = now_ms().max(self.now);
        let Some(ask) = self.details.pulls.take_ask(enabled, now) else {
            return;
        };
        let project = project.unwrap_or_default();
        match Reference::new(&project, Some(&ask.host), &ask.repository, ask.number) {
            Ok(reference) => (self.read_pull)(reference, ask.ticket),
            Err(failure) => {
                let failure = Part::Detail(Err(failure.to_string()));
                self.details.answer_pull(ask.ticket, failure, now);
                self.details.answer_pull(ask.ticket, Part::Done, now);
            }
        }
    }

    /// Hides or shows the sidebar and remembers it, as Ctrl+B and the desktop's Mod+B do. Focus
    /// on a sidebar being hidden moves to the transcript when a thread is open, or else to the
    /// composer. The rows, the highlight, the open thread and the composer stay as they are.
    fn toggle_sidebar(&mut self) {
        // A drag under way ends without its width, which the sidebar shows again next time.
        self.end_drag(false);
        self.sidebar_hidden = !self.sidebar_hidden;
        if self.sidebar_hidden && self.focus == Focus::Sidebar {
            self.focus = if self.open.is_some() {
                Focus::Transcript
            } else {
                Focus::Composer
            };
        }
        self.layout_changed = true;
        let hidden = self.sidebar_hidden;
        Settings::update(move |settings| settings.sidebar_hidden = hidden);
    }

    /// Moves focus to the sidebar, showing it first if it is hidden.
    fn focus_sidebar(&mut self) {
        if self.sidebar_hidden {
            self.toggle_sidebar();
        }
        self.focus = Focus::Sidebar;
    }

    /// How wide the sidebar draws on a screen `width` columns wide: not at all while hidden,
    /// as far as a drag has taken it during one, and otherwise as chosen or picked from the
    /// screen's width.
    fn sidebar_columns(&self, width: u16) -> u16 {
        if self.sidebar_hidden {
            return 0;
        }
        let chosen = self.drag.map(|drag| drag.width).or(self.sidebar_width);
        sidebar_width(width, chosen)
    }

    /// Starts a drag of the sidebar's edge from the width the last frame drew. The press
    /// itself changes nothing on screen.
    fn start_drag(&mut self) {
        let from = self.sidebar_columns(self.screen.width);
        self.drag = Some(Drag { from, width: from });
    }

    /// Moves the dragged edge to the pointer's column, as far as the sidebar's width allows,
    /// and says whether the sidebar's width changed.
    fn drag_to(&mut self, column: u16) -> bool {
        let screen = self.screen;
        let Some(drag) = self.drag.as_mut() else {
            return false;
        };
        // The edge is the sidebar's last column, so the width reaches one past the pointer.
        let wanted = column.saturating_add(1).saturating_sub(screen.x);
        let width = sidebar_width(screen.width, Some(wanted));
        if width == drag.width {
            return false;
        }
        drag.width = width;
        self.layout_changed = true;
        true
    }

    /// Ends a drag of the sidebar's edge, if one is under way, and says whether the screen
    /// needs drawing again. With `keep`, when the button comes up, the sidebar keeps the width
    /// it was dragged to and saves it once, if it differs from the width at the press. A press
    /// and release without a change of width saves nothing, so the width t3term picks stays
    /// its pick. Without `keep` the width at the press comes back and nothing is saved.
    fn end_drag(&mut self, keep: bool) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        if drag.width == drag.from {
            return false;
        }
        if keep {
            self.sidebar_width = Some(drag.width);
            Settings::update(move |settings| settings.sidebar_width = Some(drag.width));
            // The last frame already drew this width.
            return false;
        }
        self.layout_changed = true;
        true
    }

    /// Moves the sidebar's edge `SIDEBAR_STEP` columns right, or left when `wider` is false, as
    /// `]` and `[` do, and saves the width it lands on. The step starts from the width on
    /// screen, so it moves the edge even when the screen has narrowed the chosen width. At
    /// either limit nothing changes and nothing is saved.
    fn step_sidebar(&mut self, wider: bool) {
        if self.sidebar_hidden {
            return;
        }
        let width = self.screen.width;
        let now = self.sidebar_columns(width);
        let wanted = if wider {
            now.saturating_add(SIDEBAR_STEP)
        } else {
            now.saturating_sub(SIDEBAR_STEP)
        };
        let next = sidebar_width(width, Some(wanted));
        if next == now {
            return;
        }
        self.sidebar_width = Some(next);
        self.layout_changed = true;
        Settings::update(move |settings| settings.sidebar_width = Some(next));
    }

    /// Forgets the chosen width, as `0` in the sidebar does and a double-click on the
    /// desktop's edge does, so the sidebar takes the width t3term picks from the screen's.
    fn reset_sidebar_width(&mut self) {
        if self.sidebar_hidden || self.sidebar_width.is_none() {
            return;
        }
        self.sidebar_width = None;
        self.layout_changed = true;
        Settings::update(|settings| settings.sidebar_width = None);
    }

    /// Opens or closes `shelf` and remembers it, as the GUI does for Working. Nothing happens
    /// while the list has no heading for it. The cards below the heading move, so the next
    /// frame draws at once, and until it does a click on the list lands on nothing.
    fn toggle_shelf(&mut self, shelf: Shelf) {
        let Some(open) = self.sidebar.toggle(shelf) else {
            return;
        };
        Settings::update(move |settings| match shelf {
            Shelf::Working => settings.sidebar_working_shelf_expanded = open,
            Shelf::Done => settings.sidebar_done_shelf_expanded = open,
            _ => settings.sidebar_settled_shelf_expanded = open,
        });
        self.rebuild_rows();
        self.layout_changed = true;
    }

    fn shell_thread(&self, id: &str) -> Option<&Value> {
        sidebar::find_thread(self.shell.as_ref()?, id)
    }

    fn project_title(&self, project_id: &str) -> &str {
        sidebar::project_title(self.shell.as_ref(), project_id)
    }

    fn open_selected(&mut self) {
        let Some(id) = self.sidebar.selected_thread_id().map(str::to_string) else {
            return;
        };
        // A list that Down scrolled past the card scrolls back to it.
        self.sidebar.select(&id);
        if self.open.as_ref().is_some_and(|o| o.id == id) {
            self.focus = Focus::Composer;
            return;
        }
        self.open_by_id(id);
    }

    /// Opens thread `id` with a watch of its own, in place of the thread that was open.
    fn open_by_id(&mut self, id: String) {
        let events = self.client.watch_thread(&id, None);
        self.open_thread(OpenThread::new(id, events));
    }

    /// Opens related thread `id` from the details' Lineage, as the sidebar opens a thread. The
    /// sidebar highlights it when it lists it, and the details keep the keyboard when they had
    /// it. The open thread itself does nothing.
    fn open_related(&mut self, id: String) {
        if self.open.as_ref().is_some_and(|open| open.id == id) {
            return;
        }
        let had_keyboard = self.focus == Focus::Details;
        self.open_by_id(id.clone());
        // Opening it lays the sidebar out again, and a closed shelf lists the open thread, so
        // its row is there to highlight only now. A subagent's thread has none.
        self.sidebar.select(&id);
        if had_keyboard {
            self.focus = Focus::Details;
        }
    }

    /// Acts on a row of the details' Lineage: opens its thread, shows more of its group, or
    /// opens or closes Previous agents. A thread the shell doesn't list stays closed, and the
    /// status line says why. The shell is asked again here, since an event since the last
    /// frame can have archived or removed a thread whose row still says it opens.
    fn act_on_lineage(&mut self, target: lineage::Target) {
        match target {
            lineage::Target::Thread(id) => {
                let listed = self.shell_thread(&id).is_some_and(lineage::opens);
                if self.details.can_open(&id) && listed {
                    self.open_related(id);
                } else {
                    self.message = Some(("This related thread is unavailable.".into(), false));
                }
            }
            lineage::Target::More(group) => self.details.show_more(group),
            lineage::Target::Previous => self.details.toggle_previous(),
        }
    }

    /// Shows `open` in place of the thread that was open, as when one is picked in the sidebar.
    fn open_thread(&mut self, open: OpenThread) {
        self.open = Some(open);
        self.cache.clear();
        self.outputs.clear();
        self.open_bundles.clear();
        self.bundle_rows.clear();
        self.expanded_plans.clear();
        self.drawn_plans.clear();
        // The thread has no state yet, so the drawer closes and drops the last thread's tasks.
        self.follow_tasks();
        // The banner shows a send to this thread that failed, until the thread's own error
        // comes with it.
        self.follow_banner();
        self.follow_provider(false);
        self.follow_setup();
        // Open details show the new thread from the next frame, which works its details out
        // before it draws them.
        self.details.follow();
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

    // ---- thread details ----

    /// The main column of a screen `area`, its padding taken off, as a frame lays it out.
    fn main_column(&self, area: Rect) -> Rect {
        padded(columns(area, self.sidebar_columns(area.width)).1)
    }

    /// Opens or closes the thread's details, as Alt+B and their button do. Details that open
    /// as a dialog take the keyboard. A pane beside the conversation leaves it where it was,
    /// so typing in the composer goes on. Closing gives it back to the pane that had it.
    fn toggle_details(&mut self) {
        if self.details.open {
            self.close_details();
            return;
        }
        self.details.toggle();
        self.layout_changed = true;
        if details::layout(self.main_column(self.screen)) == details::Layout::Dialog {
            self.focus_details(true);
        }
    }

    /// Closes the details, if they are open, and gives the keyboard back from them.
    fn close_details(&mut self) {
        if self.details.open {
            self.details.toggle();
            self.layout_changed = true;
        }
        self.leave_details();
    }

    /// Gives the details the keyboard, noting the pane that had it. An open menu closes.
    /// `by_dialog` says a dialog took it, rather than the reader with a click or Tab, so the
    /// keyboard goes back if the dialog turns into a pane. A pane never takes it by itself.
    fn focus_details(&mut self, by_dialog: bool) {
        if self.focus != Focus::Details {
            self.details.return_focus = Some(self.focus);
            self.focus = Focus::Details;
            self.details.taken = by_dialog;
        } else if !by_dialog {
            self.details.taken = false;
        }
        self.picker = None;
    }

    /// Gives the keyboard back from the details to the pane that had it before them, or to the
    /// composer. A sidebar hidden since gives it to the transcript, as hiding it does.
    fn leave_details(&mut self) {
        self.details.taken = false;
        if self.focus != Focus::Details {
            return;
        }
        self.focus = match self.details.return_focus.take() {
            Some(Focus::Sidebar) if self.sidebar_hidden => {
                if self.open.is_some() {
                    Focus::Transcript
                } else {
                    Focus::Composer
                }
            }
            Some(focus @ (Focus::Sidebar | Focus::Transcript | Focus::Composer)) => focus,
            Some(Focus::Details) | None => Focus::Composer,
        };
    }

    /// Whether the details are a dialog with the keyboard and the mouse: they have focus and
    /// the last frame didn't draw them as a pane. That includes the moment between Alt+B
    /// opening them as a dialog and the frame that draws it.
    fn details_modal(&self) -> bool {
        self.focus == Focus::Details && self.details.shown() != Some(details::Layout::Pane)
    }

    /// A key for the details while they have the keyboard. Esc closes a dialog. On a pane it
    /// gives the keyboard back and leaves the pane open. → or `l` shows the thread's pull
    /// requests, and ← or `h` goes back: from a pull request's details to the list, and from
    /// the list to the thread's tab. On the thread's tab the up and down arrows, `j` and `k`
    /// choose among Lineage's rows, and scroll the text past them or without them, and Enter
    /// acts on the chosen row. On the pull requests' tab they move along the pull requests, or
    /// scroll the selected one's details, which Enter shows in the list's place and hides
    /// again. The page keys, Home, End, `g` and `G` scroll the text, the list or the details.
    fn on_details_key(&mut self, key: KeyEvent) {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc if self.details.shown() == Some(details::Layout::Pane) => {
                self.leave_details()
            }
            KeyCode::Esc => self.close_details(),
            KeyCode::Left => self.details.back(),
            KeyCode::Char('h') if plain => self.details.back(),
            KeyCode::Right => self.details.show(details::Tab::PullRequests),
            KeyCode::Char('l') if plain => self.details.show(details::Tab::PullRequests),
            KeyCode::Enter => {
                if let Some(target) = self.details.enter() {
                    self.act_on_lineage(target);
                }
            }
            KeyCode::Up => self.details.up(),
            KeyCode::Char('k') if plain => self.details.up(),
            KeyCode::Down => self.details.down(),
            KeyCode::Char('j') if plain => self.details.down(),
            KeyCode::PageUp => self.details.page(false),
            KeyCode::PageDown => self.details.page(true),
            KeyCode::Home => self.details.scroll_home(),
            KeyCode::Char('g') if plain => self.details.scroll_home(),
            KeyCode::End => self.details.scroll_end(),
            KeyCode::Char('G') if plain => self.details.scroll_end(),
            _ => {}
        }
    }

    /// The open thread's record for its details. While its watch is open, that is the
    /// projection's once the watch sends it, and the shell's copy until then. After the watch
    /// closes, the shell's copy, which still changes, comes first. A projection whose thread
    /// has another id counts as none. Without either, the thread is loading until the watch
    /// sends a projection, says it caught up or closes, and unavailable after.
    fn details_record(&self) -> details::Record<'_> {
        let Some(open) = self.open.as_ref() else {
            return details::Record::None;
        };
        let closed = open.connection == "closed";
        let answered = open
            .state
            .as_ref()
            .is_some_and(|state| state.synchronized || !state.projection.is_empty());
        let projection = open
            .state
            .as_ref()
            .map(ThreadState::thread)
            .filter(|thread| thread.get("id").and_then(Value::as_str) == Some(open.id.as_str()));
        match (projection, self.shell_thread(&open.id)) {
            (Some(thread), _) if !closed => details::Record::Found(thread),
            (_, Some(thread)) | (Some(thread), None) => details::Record::Found(thread),
            (None, None) if closed || answered => details::Record::Unavailable,
            (None, None) => details::Record::Loading,
        }
    }

    /// The open thread's Lineage, from the shell's threads and the thread's projection. Once
    /// the thread's watch closes, the shell's copy of each thread it lists comes first, as it
    /// does for the record in `details_record`.
    fn details_lineage(&self) -> lineage::Lineage {
        let open = self.open.as_ref();
        let threads = match self.shell.as_ref() {
            Some(shell) => shell.threads.as_slice(),
            None => &[],
        };
        let projection = open
            .and_then(|open| open.state.as_ref())
            .map(|state| &state.projection);
        let closed = open.is_some_and(|open| open.connection == "closed");
        lineage::Lineage::of(
            open.map(|open| open.id.as_str()),
            threads,
            projection,
            closed,
            self.details.paging(),
        )
    }

    /// The shell's projects for the details, or why it has none yet.
    fn details_projects(&self) -> details::Projects<'_> {
        match self.shell.as_ref() {
            Some(shell) => details::Projects::Listed(&shell.projects),
            None if self.feed == Feed::Failed => details::Projects::Failed,
            None => details::Projects::Loading,
        }
    }

    // ---- input ----

    /// Acts on a key, a paste or the mouse. When that opened a link's details, asks its host
    /// for the pull request.
    fn on_terminal_event(&mut self, event: Event) -> bool {
        let changed = self.on_input(event);
        self.read_opened_pull();
        changed
    }

    fn on_input(&mut self, event: Event) -> bool {
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
                if self.drag.is_some() {
                    match mouse.kind {
                        MouseEventKind::Drag(MouseButton::Left) => {
                            return self.drag_to(mouse.column);
                        }
                        // The release moves the edge to its column, as the desktop's does, and
                        // ends the drag. It does nothing else, wherever it lands.
                        MouseEventKind::Up(MouseButton::Left) => {
                            let moved = self.drag_to(mouse.column);
                            self.end_drag(true);
                            return moved;
                        }
                        // The button came up where the terminal didn't report it, such as
                        // outside the window, so the drag ends as a release would. A press
                        // then goes on to do what it does.
                        MouseEventKind::Moved => return self.end_drag(true),
                        MouseEventKind::Down(MouseButton::Left) => {
                            self.end_drag(true);
                        }
                        // The wheel and the other buttons wait for the drag to end.
                        _ => return false,
                    }
                }
                // A dialog of the thread's details takes the mouse. A click on its × closes it,
                // as does a click outside it, on the button too. A click on a tab, a pull
                // request or a Lineage row acts on it, and the wheel over the dialog scrolls it.
                // Nothing reaches what it covers. Until a frame draws it after a change, it
                // takes events and does nothing with them.
                if self.details_modal() {
                    let fresh = !self.details.moved && !self.layout_changed;
                    let Some(drawn) = self.details.drawn.filter(|_| fresh) else {
                        return false;
                    };
                    let lineage_row = self.details.target_at(mouse.column, mouse.row);
                    match mouse.kind {
                        _ if click && inside(drawn.close) => self.close_details(),
                        _ if click && inside(drawn.area) => {
                            let Some(target) = lineage_row else {
                                return self.details.click(mouse.column, mouse.row);
                            };
                            self.details.choose(target.clone());
                            self.act_on_lineage(target);
                        }
                        _ if click => self.close_details(),
                        MouseEventKind::ScrollUp if inside(drawn.area) => self.details.wheel(-1),
                        MouseEventKind::ScrollDown if inside(drawn.area) => self.details.wheel(1),
                        _ => return false,
                    }
                    return true;
                }
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
                // The left button on the sidebar's edge starts a drag of it, after a click that
                // closes a menu. The press does nothing else, so a click there leaves focus,
                // the highlight and the open thread alone.
                if click && inside(self.sidebar.border) {
                    self.start_drag();
                    return false;
                }
                if let Some(kind) = chip.filter(|_| click) {
                    self.focus = Focus::Composer;
                    self.open_picker(kind);
                    return true;
                }
                if click && inside(self.sidebar_toggle) {
                    self.toggle_sidebar();
                    return true;
                }
                // The details' button opens and closes them. Their pane takes what lands on
                // it: a click on × closes it, and a click elsewhere on it gives it the keyboard
                // and acts on the tab, the pull request or the Lineage row under it. The wheel
                // scrolls it. Until a frame draws them after a change, the button and the pane
                // take events and do nothing with them.
                let moved = self.details.moved || self.layout_changed;
                if click && inside(self.details.button) {
                    if moved {
                        return false;
                    }
                    self.toggle_details();
                    return true;
                }
                if let Some(drawn) = self
                    .details
                    .drawn
                    .filter(|drawn| drawn.layout == details::Layout::Pane && inside(drawn.area))
                {
                    if moved {
                        return false;
                    }
                    let lineage_row = self.details.target_at(mouse.column, mouse.row);
                    match mouse.kind {
                        _ if click && inside(drawn.close) => self.close_details(),
                        _ if click => {
                            self.focus_details(false);
                            if let Some(target) = lineage_row {
                                self.details.choose(target.clone());
                                self.act_on_lineage(target);
                            } else {
                                self.details.click(mouse.column, mouse.row);
                            }
                        }
                        MouseEventKind::ScrollUp => self.details.wheel(-1),
                        MouseEventKind::ScrollDown => self.details.wheel(1),
                        _ => return false,
                    }
                    return true;
                }
                // The banners lie over the transcript, so each takes what lands on it: a click
                // on × dismisses it, a click elsewhere on it opens or closes its details, and
                // the wheel scrolls them. Nothing reaches the rows under them. Opening one
                // closes the other, so only one is ever open.
                if let Some(redraw) = self.provider_banner.on_mouse(&mouse) {
                    if self.provider_banner.expanded() {
                        self.banner.collapse();
                    }
                    return redraw;
                }
                if let Some(redraw) = self.banner.on_mouse(&mouse) {
                    if self.banner.expanded() {
                        self.provider_banner.collapse();
                    }
                    self.follow_banner();
                    return redraw;
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
                    MouseEventKind::Down(MouseButton::Left)
                        if inside(self.transcript_area) && self.setup_toggle_at(mouse.row) =>
                    {
                        self.toggle_setup()
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
                    MouseEventKind::Down(MouseButton::Left)
                        if inside(self.sidebar.list)
                            && self.sidebar.shelf_at(mouse.row).is_some() =>
                    {
                        if let Some(shelf) = self.sidebar.shelf_at(mouse.row) {
                            self.toggle_shelf(shelf);
                        }
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
                // The setup card's Details row moves with the width.
                if let Some(open) = self.open.as_mut() {
                    open.setup.drawn = None;
                }
                // A drag's columns don't line up with the new screen, so it ends without its
                // width. The edge may have moved, so it takes no press until the next frame
                // draws it.
                self.end_drag(false);
                self.sidebar.border = Rect::default();
                // The details' button and the details may have moved too, so they take no
                // click until then.
                self.details.resized();
                true
            }
            _ => false,
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Status messages are toasts: the next key dismisses them.
        self.message = None;
        // A key during a drag of the sidebar's edge puts its width back first. Esc does
        // nothing else.
        if self.drag.is_some() {
            self.end_drag(false);
            if key.code == KeyCode::Esc {
                return;
            }
        }
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
            // From any pane, an open menu included, and never typed into the composer.
            KeyCode::Char('b') if ctrl => {
                self.toggle_sidebar();
                return;
            }
            // The thread's details, from any pane, an open menu included, and never typed into
            // the composer.
            KeyCode::Char('b') if alt => {
                self.toggle_details();
                return;
            }
            // The color scheme, from any pane, an open menu or dialog included. The desktop's
            // Cmd+Option+Shift+A. Terminals send Esc then `A` for it, which Alt+A below for an
            // approval doesn't take.
            KeyCode::Char('A') if alt => {
                self.cycle_appearance();
                return;
            }
            // A dialog of the details takes every other key, so Esc closes it before it moves
            // focus anywhere, and nothing reaches the conversation under it.
            _ if self.details_modal() => {
                self.on_details_key(key);
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
            // While a banner shows, from any pane and over a menu. Alt+N dismisses the provider
            // banner and Alt+O opens or closes its message. Alt+W dismisses the error banner
            // and Alt+I opens or closes its details. Opening one closes the other, so Alt+↑/↓
            // scroll whichever is open and too long to show, ahead of the request panel and
            // the tasks drawer. None of them reach the composer.
            _ if self.provider_banner.on_key(&key) => {
                if self.provider_banner.expanded() {
                    self.banner.collapse();
                }
                return;
            }
            _ if self.banner.on_key(&key) => {
                if self.banner.expanded() {
                    self.provider_banner.collapse();
                }
                self.follow_banner();
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
            KeyCode::Tab | KeyCode::BackTab => {
                let forward = key.code == KeyCode::Tab;
                let pane = self.details.shown() == Some(details::Layout::Pane);
                match cycle(self.focus, forward, self.sidebar_hidden, pane) {
                    Focus::Details => self.focus_details(false),
                    next => self.focus = next,
                }
                return;
            }
            // The page keys scroll the transcript from any pane but the details'.
            KeyCode::PageUp if self.focus != Focus::Details => {
                self.scroll.set(self.scroll.rows() + self.page());
                return;
            }
            KeyCode::PageDown if self.focus != Focus::Details => {
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
                KeyCode::Char('w') => self.toggle_shelf(Shelf::Working),
                KeyCode::Char('d') => self.toggle_shelf(Shelf::Done),
                KeyCode::Char('e') => self.toggle_shelf(Shelf::Settled),
                KeyCode::Char('[') => self.step_sidebar(false),
                KeyCode::Char(']') => self.step_sidebar(true),
                KeyCode::Char('0') => self.reset_sidebar_width(),
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
                KeyCode::Char('w') if !ctrl && !alt => self.toggle_setup(),
                KeyCode::Char('a') => self.respond("accept"),
                KeyCode::Char('s') => self.respond("acceptForSession"),
                KeyCode::Char('d') => self.respond("decline"),
                KeyCode::Char('i') | KeyCode::Enter => self.focus = Focus::Composer,
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => self.focus_sidebar(),
                _ => {}
            },
            Focus::Details => self.on_details_key(key),
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
        self.scroll.pin(Pin::Plan(id), row);
    }

    /// Opens or closes the worktree setup's details, as the Details button on the desktop's
    /// card does. Only this window keeps it. The next frame keeps the Details row on the row
    /// it was on, as a plan's toggle keeps its header, unless a key or the wheel moves the
    /// transcript first. It does nothing unless the last frame drew the card with a Details
    /// row, and a second toggle before the next frame does nothing either.
    fn toggle_setup(&mut self) {
        let Some(row) = self.open.as_mut().and_then(|open| open.setup.toggle()) else {
            return;
        };
        self.focus = Focus::Transcript;
        self.scroll.pin(Pin::Setup, row);
    }

    /// Whether screen row `row` showed the worktree setup's Details row in the last frame, and
    /// nothing has moved it since: the transcript hasn't scrolled, the setup hasn't changed,
    /// the screen hasn't resized and no sidebar change is waiting for a frame.
    fn setup_toggle_at(&self, row: u16) -> bool {
        !self.layout_changed
            && self
                .open
                .as_ref()
                .is_some_and(|open| open.setup.toggle_at(row, self.scroll.rows()))
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

    /// Turns the color scheme to the next of System, Light and Dark, as the desktop's
    /// Cmd+Option+Shift+A does, saves it and says which it is, as the desktop's toast does. Turning
    /// to System reads the system's appearance again, since it may have changed. Until the read
    /// answers, the screen keeps the appearance it has.
    fn cycle_appearance(&mut self) {
        let mode = self.appearance_mode.next();
        self.appearance_mode = mode;
        Settings::update(move |settings| settings.appearance_mode = mode);
        if mode == AppearanceMode::System {
            self.read_system_appearance();
        }
        self.follow_appearance();
        self.message = Some((format!("Appearance: {}", mode.label()), false));
    }

    /// Reads the system's appearance in the background. The answer arrives as an action with
    /// the read's number, and a read that can't tell sends nothing.
    fn read_system_appearance(&mut self) {
        self.system_reads += 1;
        let read = self.system_reads;
        let results = self.actions.clone();
        tokio::spawn(async move {
            if let Some(appearance) = appearance::system().await {
                let _ = results.send(ActionResult::SystemAppearance { appearance, read });
            }
        });
    }

    /// Draws the half of the theme the color scheme calls for, when it isn't the one drawing.
    fn follow_appearance(&mut self) {
        let current = self.theme.appearance;
        let wanted = appearance::resolve(self.appearance_mode, self.system_appearance, current);
        if wanted != current {
            self.theme = Theme::of(self.theme.name, wanted, self.theme.depth);
            self.restyle();
        }
    }

    /// Drops every row laid out in the old colors, so the next frame draws all of it in the
    /// new ones, at once.
    fn restyle(&mut self) {
        self.cache.clear();
        self.details.restyle();
        if let Some(open) = self.open.as_mut() {
            open.setup.restyle();
        }
        self.layout_changed = true;
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
            // An older read than the one kept says less about the system now.
            ActionResult::SystemAppearance { appearance, read } if read > self.system_read_kept => {
                self.system_read_kept = read;
                self.system_appearance = Some(appearance);
                self.follow_appearance();
            }
            ActionResult::SystemAppearance { .. } => {}
            ActionResult::Config { config, revision } => {
                // A change from the subscription since the read was asked for is newer than
                // the answer.
                if revision == self.config_revision {
                    self.update_config(|held| {
                        *held = Some(config);
                        true
                    });
                }
            }
            ActionResult::PullRequest { ticket, part } => {
                let now = now_ms().max(self.now);
                self.details.answer_pull(ticket, part, now);
                // A link opened while that read was under way is asked for once it is done.
                self.read_opened_pull();
            }
            ActionResult::Output {
                item_id,
                revision,
                text,
            } => {
                self.outputs.store(item_id, revision, text);
            }
            ActionResult::Sent { note } => {
                if self.settle_draft() {
                    self.follow_provider(false);
                }
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
                // The thread's banner shows the error, as the desktop's does, and keeps it
                // after the toast goes, until the next send to that thread.
                self.banner
                    .failed(&thread_id, error.clone(), message_id.clone());
                let message = Failed {
                    thread_id: thread_id.clone(),
                    message_id: Some(message_id),
                    text,
                };
                self.give_back(&thread_id, message, error);
                // The thread may have shown the message before this result arrived.
                self.drop_landed();
                self.follow_banner();
                self.follow_provider(false);
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
        // A send that failed but reached T3 anyway is no error to show.
        self.banner.landed(&open.id, |id| state.has_message(id));
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
    /// message doesn't undo a change made in another client. Returns whether it dropped it.
    fn settle_draft(&mut self) -> bool {
        let (Some(open), Some(config)) = (self.open.as_ref(), self.config.as_ref()) else {
            return false;
        };
        let (Some(draft), Some(thread)) = (self.drafts.get(&open.id), self.open_thread_json())
        else {
            return false;
        };
        if matches!(
            picker::spent(config, thread, draft, self.plan_mode_enabled),
            Ok(true)
        ) {
            let id = open.id.clone();
            self.drafts.remove(&id);
            return true;
        }
        false
    }

    // ---- model, effort and mode menus ----

    /// Reads T3's configuration once in the background. Menus use the last copy until it
    /// arrives.
    fn load_config(&self) {
        let (client, results) = (self.client.clone(), self.actions.clone());
        let revision = self.config_revision;
        tokio::spawn(async move {
            let _ = results.send(match client.server_config().await {
                Ok(config) => ActionResult::Config { config, revision },
                Err(e) => ActionResult::Error(format!("Couldn't read T3's model list: {e}")),
            });
        });
    }

    /// Changes the configuration with `change`, which says whether it changed anything, and
    /// keeps what depends on it in step. A change can add or drop menu rows, so an open menu's
    /// highlight follows its entry, not its row number. The sidebar is laid out again when the
    /// change turns snooze or settlement on or off, which decide whether those shelves apply.
    /// The provider banner follows the selected provider's new status.
    fn update_config(&mut self, change: impl FnOnce(&mut Option<Value>) -> bool) -> bool {
        let (pick, capabilities) = (self.selected_pick(), self.capabilities());
        if !change(&mut self.config) {
            return false;
        }
        if self.picker.is_some() {
            self.select_pick(pick);
        }
        if self.capabilities() != capabilities {
            self.rebuild_rows();
        }
        self.follow_provider(true);
        true
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
        // Provider status changes while T3 runs. The config subscription brings each change,
        // so a menu reads the configuration only while the TUI has none or after T3 refused
        // the subscription.
        if self.picker.is_none() && (self.config.is_none() || !self.config_subscribed) {
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
        self.follow_provider(false);
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
        // A new send clears the last one's error, as the desktop's does. If this one fails
        // too, its error takes the place.
        self.banner.clear_local(&thread_id);
        self.follow_banner();
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
        self.follow_provider(false);
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

    /// The connection the status line names: the open thread's watch, or the sidebar's feed
    /// while no thread is open. The sidebar's dot is always the feed's.
    fn connection_state(&self) -> (String, Color) {
        let t = &self.theme;
        let connection = match self.open.as_ref() {
            Some(open) => open.connection.clone(),
            None => self.feed.word().to_string(),
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
        self.layout_changed = false;
        self.screen = area;
        let (sidebar, main) = columns(area, self.sidebar_columns(area.width));
        // The main column keeps one blank column on each side, like the GUI's padding.
        let main = padded(main);
        // How the details fit decides the conversation's width. A dialog has the keyboard, so
        // it takes it before the sidebar and the composer draw whether they have it. A pane it
        // turns into gives it back.
        let shown = self.details.open.then(|| details::layout(main));
        match shown {
            Some(details::Layout::Dialog) => self.focus_details(true),
            Some(details::Layout::Pane) if self.details.taken => self.leave_details(),
            Some(details::Layout::Pane) => {}
            None => self.leave_details(),
        }
        // The details and the pull requests linked to the thread are worked out again only
        // after an event that can change them, or for another thread, and wrapped again only at
        // another width. That comes before the status line, which says what Enter does with the
        // Lineage row they keep chosen. When T3 removes the link whose details show, the first
        // frame without them offers the list's keys.
        let open_id = self.open.as_ref().map(|open| open.id.as_str());
        if self.details.wants(open_id) {
            let facts = details::Facts::of(open_id, self.details_record(), self.details_projects());
            self.details.set(facts);
        }
        if self.details.wants_lineage(open_id) {
            let lineage = self.details_lineage();
            self.details.set_lineage(lineage);
        }
        let supported = self.links_supported();
        if self.details.wants_pulls(open_id, supported) {
            let list = pull_requests::List::of(open_id, self.details_record(), supported);
            self.details.pulls.set(list);
        }
        // The wake timer usually moves a woken thread first, but it runs on a clock that
        // stops while the machine sleeps. A hidden sidebar keeps its rows current too, so it
        // comes back as T3 has it.
        if self.sidebar.next_wake.is_some_and(|wake| wake <= self.now) {
            self.rebuild_rows();
        }
        if self.sidebar_hidden {
            self.sidebar.forget_drawn();
        } else {
            let view = View {
                theme: &self.theme,
                shell: self.shell.as_ref(),
                badges: &self.badges,
                open_id: self.open.as_ref().map(|o| o.id.as_str()),
                focused: self.focus == Focus::Sidebar,
                now: self.now,
                feed: self.feed,
                environment: &self.environment,
            };
            self.sidebar.draw(frame, sidebar, &view);
        }

        // Beside the details' pane, the conversation narrows. The title row and the status line
        // keep the main column's width.
        let conversation = match shown {
            Some(details::Layout::Pane) => details::beside(main),
            _ => main,
        };
        let (composer_rows, cursor) = self
            .composer
            .layout(conversation.width.saturating_sub(4) as usize);
        // Text rows, a spacer, the chips row and two borders.
        let composer_height = composer_rows.len().clamp(1, COMPOSER_MAX_ROWS) as u16 + 4;
        // The header has the title row and a blank row under it, and a blank row over it on a
        // screen with room, level with the sidebar's over the wordmark.
        let inset = top_inset(area.height);
        let header_height = 2 + inset;
        // The panel leaves the header, three transcript rows, the composer and the status line
        // their space, less its own two borders.
        let panel_width = conversation.width.saturating_sub(4) as usize;
        let panel_room = main
            .height
            .saturating_sub(header_height + 3 + composer_height + 1 + 2);
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
            .saturating_sub(header_height + 3 + composer_height + 1 + panel_height);
        let tasks = self.tasks.lay_out(
            conversation.width.saturating_sub(4) as usize,
            tasks_room as usize,
            area.height as usize,
            &self.theme,
        );
        let [header, body, panel_area, drawer, composer_area, status_area] = Layout::vertical([
            Constraint::Length(header_height),
            Constraint::Min(3),
            Constraint::Length(panel_height),
            Constraint::Length(tasks.len() as u16),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .areas(conversation);
        let header = Rect {
            y: header.y + inset.min(header.height),
            width: main.width,
            height: header.height.saturating_sub(inset),
            ..header
        };
        let status_area = Rect {
            width: main.width,
            ..status_area
        };

        // The toggle leads the header, where the desktop's sits in its title bar, so a hidden
        // sidebar always has a way back. The breadcrumb follows it.
        let toggle = Rect::new(header.x, header.y, TOGGLE.width() as u16, 1);
        self.sidebar_toggle = toggle.intersection(header);
        frame.render_widget(
            Paragraph::new(Line::styled(
                TOGGLE,
                Style::new().fg(self.theme.muted).bg(self.theme.chip_bg),
            )),
            self.sidebar_toggle,
        );
        // The details' button ends the row, as the desktop's ends its title bar, when the row
        // has room for both buttons.
        let button_width = details::BUTTON.width() as u16;
        let button_room = button_width + 1;
        self.details.button = if header.width >= self.sidebar_toggle.width + 1 + button_room {
            Rect::new(header.right() - button_width, header.y, button_width, 1)
        } else {
            Rect::default()
        };
        let button_style = if self.details.open {
            Style::new()
                .fg(self.theme.focus)
                .bg(self.theme.chip_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(self.theme.muted).bg(self.theme.chip_bg)
        };
        frame.render_widget(
            Paragraph::new(Line::styled(details::BUTTON, button_style)),
            self.details.button,
        );
        let taken = if self.details.button.width > 0 {
            button_room
        } else {
            0
        };
        let breadcrumb = Rect {
            x: header.x + self.sidebar_toggle.width + 1,
            width: header
                .width
                .saturating_sub(self.sidebar_toggle.width + 1 + taken),
            ..header
        };
        // A hidden sidebar, or one too narrow for its dot, leaves the environment to the
        // breadcrumb's row.
        let with_environment = !self.sidebar.drew_environment();
        frame.render_widget(
            Paragraph::new(self.header_line(breadcrumb.width as usize, with_environment)),
            breadcrumb,
        );
        self.transcript_area = body;
        self.draw_transcript(frame, body);
        // Over the transcript's top rows, which keep their place under them.
        self.draw_banners(frame, body);
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
                height: composer_area.y.saturating_sub(conversation.y),
                ..conversation
            };
            self.draw_picker(frame, above);
        }
        frame.render_widget(
            Paragraph::new(self.status_line(status_area.width as usize, shown)),
            status_area,
        );
        let now = self.now;
        match shown {
            Some(details::Layout::Pane) => {
                let pane = details::pane_area(main, body.y, composer_area.bottom());
                let focused = self.focus == Focus::Details;
                self.details
                    .draw_pane(frame, pane, focused, now, &self.environment, &self.theme);
            }
            Some(details::Layout::Dialog) => {
                // Under the title row, wherever the inset puts it.
                let below = Rect {
                    y: area.y + inset,
                    height: area.height - inset,
                    ..area
                };
                self.details
                    .draw_dialog(frame, below, now, &self.environment, &self.theme)
            }
            None => self.details.forget_drawn(),
        }
    }

    /// The GUI's breadcrumb: project, a slash, the thread title, and the open thread's watch at
    /// the end while it isn't live. With `with_environment`, when the sidebar doesn't show it, the
    /// row ends with the feed's dot and the environment, as `environment::chip` lays them out,
    /// in at most a third of the row and what the title leaves after its first `KEPT_TITLE`
    /// columns.
    fn header_line(&self, width: usize, with_environment: bool) -> Line<'static> {
        let t = &self.theme;
        let muted = Style::new().fg(t.muted);
        // The breadcrumb borrows the titles as T3 sent them and reads only what it has room for.
        let (mut left, title, title_style, mut right) = match self.open.as_ref() {
            None => (
                Vec::new(),
                "Pick a thread in the sidebar",
                muted,
                Vec::new(),
            ),
            Some(open) => match self.open_thread_json() {
                None => (Vec::new(), "Loading…", muted, Vec::new()),
                Some(thread) => {
                    let project_id = str_of(thread, "projectId");
                    let badge = self.badges.get(project_id);
                    let bold = Style::new()
                        .fg(t.project_color(badge))
                        .add_modifier(Modifier::BOLD);
                    // The project's name is cut to the row's width, more than the row has after
                    // the badge, so a long name runs off the row as it did whole.
                    let project = label::cut(self.project_title(project_id), width);
                    let left = vec![
                        Span::styled(badge.letters.clone(), bold),
                        Span::styled(format!(" {project}"), muted),
                        Span::styled("  /  ", Style::new().fg(t.border_strong)),
                    ];
                    let right = if open.connection == "live" {
                        Vec::new()
                    } else {
                        vec![Span::styled(
                            open.connection.clone(),
                            Style::new().fg(t.warning_fg),
                        )]
                    };
                    (left, str_of(thread, "title"), Style::new().fg(t.fg), right)
                }
            },
        };
        let left_width: usize = left.iter().map(|s| s.content.width()).sum();
        // The title leaves a column spare before what follows it, as it did when `fit` could run
        // a column over, so the row keeps its layout.
        let lead = left_width + usize::from(!left.is_empty());
        if with_environment {
            let state: usize = right.iter().map(|s| s.content.width()).sum();
            let kept = label::columns(title, KEPT_TITLE);
            let room = width.saturating_sub(lead + state + kept).min(width / 3);
            right.extend(environment::chip(&self.environment, self.feed, room, t));
        }
        let right_width: usize = right.iter().map(|s| s.content.width()).sum();
        let room = width.saturating_sub(lead + right_width);
        // With no room, the `…` alone takes that spare column.
        let shown = if room == 0 && !title.is_empty() {
            String::from("…")
        } else {
            label::cut(title, room)
        };
        left.push(Span::styled(shown, title_style));
        row(left, right, width)
    }

    /// Draws the provider banner over the top rows of `body`, the transcript's, and the thread
    /// error banner under it, as the desktop stacks the two. Closed, they take at most half of
    /// the rows between them. With an error to show, the provider banner leaves the error banner
    /// half of those rows, and at least three, so on a transcript of fewer than eight rows only
    /// the error shows. An open banner can take every row the other leaves, and the error
    /// banner keeps what it would have when closed.
    fn draw_banners(&mut self, frame: &mut Frame, body: Rect) {
        let height = body.height as usize;
        let half = (height / 2).max(1);
        let kept = if self.banner.prints() {
            half.min(3).max(half / 2)
        } else {
            0
        };
        let limit = if self.provider_banner.expanded() {
            height
        } else {
            half
        };
        self.provider_banner
            .draw(frame, body, limit.saturating_sub(kept), &self.theme);
        let taken = self.provider_banner.area.height;
        let below = Rect {
            y: body.y + taken,
            height: body.height - taken,
            ..body
        };
        let room = below.height as usize;
        let limit = if self.banner.expanded() {
            room
        } else if self.provider_banner.expanded() {
            half.min(room)
        } else {
            half.saturating_sub(taken as usize).min(room)
        };
        self.banner.draw_limited(frame, below, limit, &self.theme);
    }

    fn draw_transcript(&mut self, frame: &mut Frame, area: Rect) {
        let t = self.theme.clone();
        let muted = Style::new().fg(t.muted);
        // Clicks and `p` act on the plans this frame draws, so the last frame's go first.
        self.drawn_plans.clear();
        let width = area.width.saturating_sub(1).max(10);
        let now = self.now;
        if let Some(open) = self.open.as_mut() {
            open.prepare();
            // Clicks and `w` act on the setup card this frame draws too.
            open.setup.drawn = None;
            open.lay_out_setup(width as usize, now, &t);
        }
        let Some((state, run_changes, prepared, setup)) = self
            .open
            .as_ref()
            .and_then(|o| Some((o.state.as_ref()?, &o.run_changes, &o.prepared, &o.setup)))
        else {
            let hint = if self.open.is_some() {
                "Loading thread…"
            } else if self.sidebar_hidden {
                "Press Ctrl+B to show the sidebar and pick a thread."
            } else {
                "Select a thread with ↑/↓ and press Enter."
            };
            frame.render_widget(Paragraph::new(Line::styled(hint, muted)), area);
            return;
        };
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
        // The worktree setup card goes under the thread's first prompt and its clock row, as the
        // desktop puts it under the first message and its working row, or after the last block
        // while there is no prompt. Under a prompt its rows count as the prompt's, so the anchor
        // and the pins treat them as the prompt's own. After the last block they count as a
        // height of their own, so the scroll's total has them.
        let card = setup.card();
        let host = card.and(prepared.first_user);
        let card_at = card.map(|card| match host {
            Some(host) => {
                let at: usize = heights[..=host].iter().sum();
                heights[host] += card.lines.len();
                at
            }
            None => {
                let at: usize = heights.iter().sum();
                heights.push(card.lines.len());
                at
            }
        });
        let total: usize = heights.iter().sum();
        let height = area.height as usize;
        // A plan just expanded or collapsed keeps its header on the row it had, so the click
        // doesn't move the text, even while the view follows the bottom, and the setup card's
        // Details row does the same when it opens or closes. A key or the wheel that moved the
        // transcript after the toggle has already cancelled this.
        let pinned = self.scroll.take_pin().and_then(|(pin, row)| match pin {
            Pin::Plan(id) => {
                let header = self.cache.get(&id)?.toggle?.0;
                scroll_to(blocks, &heights, &id, header, row, height)
            }
            Pin::Setup => Some(scroll_at(total, card_at? + card?.toggle?, row, height)),
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
        // The setup card's rows in view, counted from its first.
        let card_view = card.zip(card_at).and_then(|(card, at)| {
            let from = start.saturating_sub(at);
            let to = end.saturating_sub(at).min(card.lines.len());
            (from < to).then_some((card, at, from, to))
        });
        let mut drawn_setup = None;

        // Copy only the rows in view.
        let mut lines = Vec::with_capacity(height);
        let mut offset = 0;
        // Verbose mode shows what a tool printed, which T3 hands over one item at a time. Only
        // the rows on screen ask for it, so opening a long thread doesn't fetch all of it.
        let mut wanted = Vec::new();
        let mut anchor = None;
        let mut rows = Vec::new();
        let mut plans = Vec::new();
        for (index, ((block, block_height), bundle)) in
            blocks.iter().zip(&heights).zip(&bundles).enumerate()
        {
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
                // A prompt with the setup card under it has the card's rows after its own.
                let own = cached.lines.len();
                lines.extend(cached.lines[from.min(own)..to.min(own)].iter().cloned());
                if host == Some(index)
                    && let Some((card, at, from, to)) = card_view
                {
                    let top = at as isize - start as isize;
                    drawn_setup = card.drawn(from, to, top, area.y, scroll);
                    lines.extend(card.lines[from..to].iter().cloned());
                }
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
        // A card with no prompt above it follows the last block.
        if host.is_none()
            && let Some((card, at, from, to)) = card_view
        {
            let top = at as isize - start as isize;
            drawn_setup = card.drawn(from, to, top, area.y, scroll);
            lines.extend(card.lines[from..to].iter().cloned());
        }
        self.bundle_rows = rows;
        self.drawn_plans = plans;
        frame.render_widget(Paragraph::new(lines), area);
        // A terminal too short for any transcript rows has nowhere to put the label.
        if scroll > 0 && area.height > 0 {
            let label = format!(" ↓ {scroll} more ");
            let x = area.x + area.width.saturating_sub(label.width() as u16 + 1);
            frame.render_widget(
                Paragraph::new(Line::styled(
                    label.clone(),
                    Style::new().fg(t.on_primary).bg(t.primary),
                )),
                Rect::new(x, area.y + area.height - 1, label.width() as u16, 1),
            );
        }
        self.anchor = anchor.filter(|_| scroll > 0);
        self.drawn_scroll = scroll;
        if let Some(open) = self.open.as_mut() {
            open.setup.drawn = drawn_setup;
        }
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
                    .fg(t.on_primary)
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
                .fg(t.on_primary)
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
                            Span::styled(check, Style::new().fg(t.focus)),
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

    /// The status line, `width` columns wide, for a frame that draws the details as `shown`.
    /// The details draw after it, so the last frame's layout may be another one after a resize.
    fn status_line(&self, width: usize, shown: Option<details::Layout>) -> Line<'static> {
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
        // Only Ctrl+B and the toggle bring a hidden sidebar back, so the keys lead with it.
        let restore = if self.sidebar_hidden && self.picker.is_none() {
            "Ctrl+B sidebar · "
        } else {
            ""
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
        let hint;
        let keys = match (self.focus, self.picker.as_ref().map(|p| p.kind)) {
            (_, Some(Kind::Model)) => "Type to search · ↑↓ choose · Enter select · Esc close",
            (_, Some(_)) => "↑↓ choose · Enter select · Esc close",
            (Focus::Sidebar, None) => {
                let working = if self.sidebar.has_shelf(Shelf::Working) {
                    "w working · "
                } else {
                    ""
                };
                let folds = if self.sidebar.has_shelf(Shelf::Done) {
                    "d done · e settled · "
                } else {
                    ""
                };
                hint = format!(
                    "↑↓ select · Enter open · {working}{folds}[ ] width · Ctrl+B hide · Tab focus · q quit"
                );
                hint.as_str()
            }
            (Focus::Composer, None) => {
                "Enter send · Alt+Enter newline · Alt+M model · Alt+E effort · Alt+P mode · Ctrl+X interrupt"
            }
            // The details' keys shorten to the room there is, so Esc still shows. Esc closes
            // details this frame draws as a dialog and leaves a pane.
            (Focus::Details, None) => {
                let modal = shown != Some(details::Layout::Pane);
                let choices = self.details.keys(modal);
                hint = fitting(&choices, room.saturating_sub(restore.width())).to_string();
                hint.as_str()
            }
            (Focus::Transcript, None) => {
                let plan = self
                    .drawn_plans
                    .first()
                    .map(|card| self.expanded_plans.contains(&card.id));
                let setup = self.open.as_ref().and_then(|open| open.setup.hint());
                hint = transcript_keys(self.verbose, plan, setup);
                hint.as_str()
            }
        };
        if room > 0 {
            let keys = format!("{restore}{keys}");
            spans.push(Span::styled(fit(&keys, room), Style::new().fg(t.muted)));
        }
        Line::from(spans)
    }
}

/// The keys the status line offers while the transcript has focus. `plan` is whether the
/// first long plan in view, the one `p` toggles, is expanded, when there is one. `setup` is
/// whether the worktree setup's details are open, while its Details row is drawn.
fn transcript_keys(verbose: bool, plan: Option<bool>, setup: Option<bool>) -> String {
    let plan = match plan {
        Some(true) => " · p collapse plan",
        Some(false) => " · p expand plan",
        None => "",
    };
    let setup = match setup {
        Some(true) => " · w hide setup details",
        Some(false) => " · w show setup details",
        None => "",
    };
    let calls = if verbose {
        "t close them"
    } else {
        "t open all"
    };
    format!("↑↓/PgUp scroll · G bottom{plan}{setup} · click a row of calls · {calls} · Esc sidebar")
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
            return Some(scroll_at(total, top, row, height));
        }
        above += block_height;
    }
    None
}

/// The scroll, in rows up from the bottom of a transcript `total` rows long, that puts its row
/// `at` on screen row `row`, which is negative above the screen.
fn scroll_at(total: usize, at: usize, row: isize, height: usize) -> usize {
    total.saturating_sub(at.saturating_add_signed(-row) + height)
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

/// The first of `choices` that fits in `room` columns, or else the last, which `fit` cuts.
fn fitting(choices: &[String], room: usize) -> &str {
    let fits = choices.iter().find(|choice| choice.width() <= room);
    fits.or(choices.last()).map_or("", String::as_str)
}

/// `text` cut with `…` to at most `width` columns, at a place `transcript::clusters` finds, so
/// an accent stays on its letter and a wide character never pushes the line past `width`.
/// `fit` can run a column over after a wide character. The sidebar and the tasks drawer allow
/// for that, so it stays as it is.
fn clip(text: &str, width: usize) -> String {
    let pieces = transcript::clusters(text);
    if pieces.iter().map(|piece| piece.width()).sum::<usize>() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    // The … takes the last column.
    let mut out = String::new();
    let mut used = 0;
    for piece in pieces {
        used += piece.width();
        if used >= width {
            break;
        }
        out.push_str(piece);
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
        // An edit of several files is about all of them, so the argument counts them, as the
        // nightly's `session-logic.ts` labels it `Changed 3 files`. T3's counts are for the
        // whole item, so they stay on the row rather than go with one path.
        "file_change" => {
            let argument = match &block.change {
                Some(change) if change.operations > 1 => {
                    let files = format!("{} files", change.operations);
                    match change.counts.as_str() {
                        "" => files,
                        counts => format!("{files}  {counts}"),
                    }
                }
                _ => block.header.trim_start_matches("edit ").to_string(),
            };
            ("✎", described("Edit"), argument)
        }
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
            // An edit's lines come cleaned and bounded from `describe`. Only their color is
            // chosen here.
            if let Some(change) = &block.change {
                for (kind, line) in &change.lines {
                    let style = match kind {
                        ChangeLine::Operation | ChangeLine::Context => muted,
                        ChangeLine::Note => muted.add_modifier(Modifier::ITALIC),
                        ChangeLine::Meta => muted.add_modifier(Modifier::BOLD),
                        ChangeLine::Hunk => Style::new().fg(t.info_fg),
                        ChangeLine::Added => Style::new().fg(t.success),
                        ChangeLine::Removed | ChangeLine::Error => Style::new().fg(t.error_fg),
                    };
                    lines.push(Line::from(vec![
                        Span::styled("  │ ", Style::new().fg(t.border_strong)),
                        Span::styled(clip(line, width.saturating_sub(4)), style),
                    ]));
                }
            }
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
            let handoff = block.item_type == "handoff";
            // A compaction still under way has its label in blue, where the desktop shimmers
            // it, so the row changes only when the item does. A failed handoff has its label in
            // red, the desktop's danger tone, and its endpoints stay grey, as on mobile.
            let label = match block.status.as_str() {
                "pending" | "running" | "waiting" if compaction => Style::new().fg(t.info),
                "failed" if handoff => Style::new().fg(t.error_fg),
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
            // Of the notices, only a compaction and a handoff have detail: the counts a
            // compaction's label leaves out and its summary, and where a handoff went.
            // `describe` has already bounded and cleaned both.
            if (compaction || handoff) && !block.body.is_empty() {
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
    use crate::settings::ThemeName;

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
            change: None,
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
    fn an_edit_lists_its_operations_error_and_patch_under_its_row() {
        let theme = Theme::new(Depth::TrueColor);
        let context = RenderContext {
            theme: &theme,
            text: Styles::new(&theme, theme.text()),
            bubble: Styles::new(&theme, theme.text()),
            reasoning: Styles::new(&theme, Style::new().fg(theme.muted)).dimmed(),
        };
        // An ACP edit of two files as the nightly sends it while it runs, with `fields` replaced
        // or added, described as the TUI's prepare step describes it.
        let edit = |fields: Value| {
            let mut item = json!({
                "id": "edit-1",
                "runId": "r1",
                "type": "file_change",
                "ordinal": 2,
                "status": "running",
                "title": null,
                "fileName": "/workspace/new.ts",
                "additions": 4,
                "deletions": 2,
                "changes": [
                    {"operation": "move", "path": "/workspace/new.ts",
                        "oldPath": "/workspace/old.ts", "fileType": "text"},
                    {"operation": "add", "path": "/workspace/日本/👩\u{200d}💻.md"},
                ],
                "updatedAt": "2026-10-09T10:00:00.000Z",
            });
            if let (Some(item), Some(fields)) = (item.as_object_mut(), fields.as_object()) {
                item.extend(fields.clone());
            }
            transcript::describe(&item, &[]).expect("an edit has a row")
        };
        let running = edit(json!({}));

        // Closed, the group's row stands for the edit. Opened, the edit's row counts its files,
        // since T3's counts are for all of them, and each operation has a line.
        let heads = bundle_tools(std::slice::from_ref(&running));
        let head = heads[0].clone().expect("a group of one");
        assert_eq!((head.count, head.names.as_str()), (1, "Edit"));
        let bundle = |open: bool| {
            Extra::Bundle(Bundle {
                count: head.count,
                names: head.names.clone(),
                open,
            })
        };
        let closed = render_block(&running, 60, &context, &bundle(false));
        assert_eq!(text(&closed), vec!["› 1 tool call · Edit"]);
        let open = render_block(&running, 60, &context, &bundle(true));
        assert_eq!(
            text(&open),
            vec![
                "⌄ 1 tool call · Edit",
                "✎ Edit   2 files  +4 -2 ",
                "  │ move /workspace/old.ts → /workspace/new.ts (text)",
                "  │ add /workspace/日本/👩\u{200d}💻.md",
            ]
        );
        assert_eq!(open[1].spans[0].style.fg, Some(theme.info));
        assert_eq!(open[2].spans[0].style.fg, Some(theme.border_strong));
        assert_eq!(open[2].spans[1].style.fg, Some(theme.muted));

        // T3 sends the edit again, failed, with the provider's error where a patch would be. The
        // error comes first, in the failure's color.
        let failed = edit(json!({
            "status": "failed",
            "diffStr": "String to replace not found",
        }));
        let lines = render_block(&failed, 60, &context, &Extra::None);
        assert_eq!(
            text(&lines),
            vec![
                "✎ Edit   2 files  +4 -2 ",
                "  │ String to replace not found",
                "  │ move /workspace/old.ts → /workspace/new.ts (text)",
                "  │ add /workspace/日本/👩\u{200d}💻.md",
            ]
        );
        assert_eq!(lines[0].spans[0].style.fg, Some(theme.error_fg));
        assert_eq!(lines[1].spans[1].style.fg, Some(theme.error_fg));

        // A server that sends the patch of an edit that didn't fail, which the pinned nightly
        // never does. Each kind of line has its own color, and the line after the hunk's counts
        // run out is a header again.
        let patch = [
            "--- a/src/a.rs",
            "+++ b/src/a.rs",
            "@@ -1,2 +1,2 @@",
            " fn main() {",
            "-    old();",
            "+    new();",
            "\\ No newline at end of file",
        ];
        let patched = edit(json!({
            "status": "completed",
            "fileName": "src/a.rs",
            "additions": 1,
            "deletions": 1,
            "changes": null,
            "diffStr": patch.join("\n"),
        }));
        let lines = render_block(&patched, 60, &context, &Extra::None);
        let rows = text(&lines);
        assert_eq!(rows[0], "✎ Edit   src/a.rs  +1 -1 ");
        assert_eq!(rows[1..], patch.map(|line| format!("  │ {line}")));
        let muted = Style::new().fg(theme.muted);
        let styles: Vec<Style> = lines[1..].iter().map(|l| l.spans[1].style).collect();
        assert_eq!(
            styles,
            [
                muted.add_modifier(Modifier::BOLD),
                muted.add_modifier(Modifier::BOLD),
                Style::new().fg(theme.info_fg),
                muted,
                Style::new().fg(theme.error_fg),
                Style::new().fg(theme.success),
                muted.add_modifier(Modifier::BOLD),
            ]
        );

        // An edit that lists no changes and sends no patch has its file and counts and nothing
        // under them.
        let plain = edit(json!({
            "status": "completed",
            "fileName": "src/main.rs",
            "changes": null,
        }));
        let lines = render_block(&plain, 60, &context, &Extra::None);
        assert_eq!(text(&lines), vec!["✎ Edit   src/main.rs  +4 -2 "]);

        // T3's description of the edit is its action, as for any tool. With one operation, the
        // argument is the file.
        let described = edit(json!({"title": "Apply the rename"}));
        assert_eq!(tool_row(&described).1, "Apply the rename");
        let single = edit(json!({
            "changes": [{"operation": "modify", "path": "/workspace/new.ts"}],
        }));
        assert_eq!(
            tool_row(&single),
            ("✎", "Edit".into(), "/workspace/new.ts  +4 -2".into())
        );

        // However narrow the transcript, no row runs past it. A long line ends in …, and a wide
        // character or joined emoji that doesn't fit before it is left out whole.
        for width in [10, 11, 23, 24, 40, 80] {
            for block in [&running, &failed, &patched, &plain] {
                let rows = text(&render_block(block, width, &context, &Extra::None));
                assert!(rows.iter().all(|row| row.width() <= width), "{rows:?}");
            }
        }
        let rows = text(&render_block(&running, 40, &context, &Extra::None));
        assert_eq!(rows[1], "  │ move /workspace/old.ts → /workspace…");
        let rows = text(&render_block(&running, 24, &context, &Extra::None));
        assert_eq!(rows[2], "  │ add /workspace/日本…");
        let rows = text(&render_block(&running, 23, &context, &Extra::None));
        assert_eq!(rows[2], "  │ add /workspace/日…");
        assert_eq!(clip("cafe\u{301}!", 5), "cafe\u{301}!");
        assert_eq!(clip("cafe\u{301}!", 4), "caf…");
        assert_eq!(clip("a👩\u{200d}💻b", 4), "a👩\u{200d}💻b");
        assert_eq!(clip("a👩\u{200d}💻b", 3), "a…");
        assert_eq!(clip("anything", 0), "");
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
                badges: &Badges::of(&shell),
                open_id: None,
                focused: false,
                now: now_ms(),
                feed: Feed::Live,
                environment: &Identity::default(),
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
            error: banner::Derived::default(),
            setup: setup::Watch::default(),
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
            Sidebar::with_choices(ShelfChoices {
                working_open: false,
                ..ShelfChoices::default()
            }),
            vec![thread("busy", json!({}))],
        );
        assert!(!closed.drew_working());
        assert!(!needs_tick(None, &closed));
        let open_shelf = drawn_by(
            Sidebar::with_choices(ShelfChoices::default()),
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
            transcript_keys(false, None, None),
            "↑↓/PgUp scroll · G bottom · click a row of calls · t open all · Esc sidebar"
        );
        assert_eq!(
            transcript_keys(true, None, None),
            "↑↓/PgUp scroll · G bottom · click a row of calls · t close them · Esc sidebar"
        );
        assert!(
            transcript_keys(false, Some(false), None).contains("G bottom · p expand plan · click")
        );
        assert!(
            transcript_keys(true, Some(true), None).contains("G bottom · p collapse plan · click")
        );
        // `w` shows while the setup card's Details row is drawn, after `p`.
        assert!(
            transcript_keys(false, Some(false), Some(false))
                .contains("p expand plan · w show setup details · click")
        );
        assert!(
            transcript_keys(false, None, Some(true))
                .contains("G bottom · w hide setup details · click")
        );
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

    /// A handoff from codex_personal to claudeAgent in run `r2`, with no title and without the
    /// models newer servers stamp on it, so the runs name both ends.
    fn handoff_item(id: &str, ordinal: u64, status: &str) -> Value {
        json!({
            "id": id,
            "threadId": "t",
            "runId": "r2",
            "type": "handoff",
            "ordinal": ordinal,
            "status": status,
            "title": null,
            "fromProviderInstanceIds": ["codex_personal"],
            "toProviderInstanceId": "claudeAgent",
            "strategy": "full_thread_summary",
            "summary": "Full conversation context for provider handoff.",
            "updatedAt": "2026-10-08T10:00:00.000Z",
        })
    }

    /// A run on `instance` with `model`.
    fn run_on(id: &str, ordinal: u64, status: &str, instance: &str, model: &str) -> Value {
        json!({"id": id, "ordinal": ordinal, "status": status, "providerInstanceId": instance,
            "modelSelection": {"instanceId": instance, "model": model}})
    }

    #[test]
    fn a_handoff_marker_names_its_endpoints_and_follows_its_item() {
        let theme = Theme::new(Depth::TrueColor);
        let context = RenderContext {
            theme: &theme,
            text: Styles::new(&theme, theme.text()),
            bubble: Styles::new(&theme, theme.text()),
            reasoning: Styles::new(&theme, Style::new().fg(theme.muted)).dimmed(),
        };
        // A handoff the runs name, and one T3 stamped with made-up models: an Esc, CJK and a
        // joined emoji.
        let running = handoff_item("h1", 3, "running");
        let mut stamped = handoff_item("h2", 4, "completed");
        stamped["title"] = json!("Imported context");
        stamped["fromModelSelections"] = json!([
            {"instanceId": "codex_personal", "model": "gpt\u{1b}[2J-5.5"},
            {"instanceId": "codex_personal", "model": "日本語"},
        ]);
        stamped["toModel"] = json!("👩\u{200d}💻 coder");
        let runs = json!([
            run_on("r1", 1, "completed", "codex_personal", "gpt-5.5"),
            run_on("r2", 2, "running", "claudeAgent", "claude-fable-5"),
        ]);
        let mut open = OpenThread::new("t".into(), mpsc::unbounded_channel().1);
        let snapshot = json!({"kind": "snapshot", "snapshotSequence": 1, "projection": {
            "thread": {"id": "t"},
            "runs": runs,
            "turnItems": [running, stamped],
        }});
        assert_eq!(open.apply(&snapshot), Applied::Snapshot);
        open.prepare();
        let first = open.prepared.blocks.clone();
        assert_eq!(first.len(), 2);
        let marker = format!("{} Context handoff {}", "─".repeat(11), "─".repeat(12));
        let rows = [
            String::new(),
            marker,
            "  gpt-5.5 → claude-fable-5".to_string(),
            String::new(),
        ];
        let lines = render_block(&first[0], 40, &context, &Extra::None);
        assert_eq!(text(&lines), rows);
        assert_eq!(lines[1].spans[1].style.fg, Some(theme.muted));
        assert_eq!(lines[2].style.fg, Some(theme.muted));

        // T3 sends the item again, failed. It stays one row, its label turns red and its
        // endpoints stay grey.
        let mut failed = running;
        failed["status"] = json!("failed");
        failed["updatedAt"] = json!("2026-10-08T10:00:05.000Z");
        let update = json!({"kind": "event", "sequence": 2, "event": {
            "type": "turn-item.updated",
            "payload": failed,
        }});
        assert_eq!(
            open.apply(&update),
            Applied::Event("turn-item.updated".into())
        );
        open.prepare();
        let blocks = &open.prepared.blocks;
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].item_id, "h1");
        assert_eq!(blocks[0].status, "failed");
        let lines = render_block(&blocks[0], 40, &context, &Extra::None);
        assert_eq!(text(&lines), rows);
        assert_eq!(lines[1].spans[1].style.fg, Some(theme.error_fg));
        assert_eq!(lines[2].style.fg, Some(theme.muted));
        // A reconnect replays the event, which changes nothing.
        let made = open.prepared.made;
        assert_eq!(open.apply(&update), Applied::Duplicate);
        open.prepare();
        assert_eq!(open.prepared.blocks.len(), 2);
        assert_eq!(open.prepared.made, made);

        // The stamped handoff keeps its models in order, cleaned. At 10 columns, the narrowest
        // the transcript is drawn, the label is cut and the endpoints wrap under it with the
        // joined emoji whole.
        let imported = &open.prepared.blocks[1];
        assert_eq!(imported.body, "gpt[2J-5.5, 日本語 → 👩\u{200d}💻 coder");
        let rows = text(&render_block(imported, 10, &context, &Extra::None));
        assert_eq!(
            rows,
            [
                "",
                "── Con… ──",
                "  gpt[2J",
                "  -5.5,",
                "  日本語",
                "  → 👩\u{200d}💻",
                "  coder",
                "",
            ]
        );
        for width in [10, 11, 13] {
            for block in &open.prepared.blocks {
                let rows = text(&render_block(block, width, &context, &Extra::None));
                assert!(rows.iter().all(|row| row.width() <= width), "{rows:?}");
            }
        }

        // Another notice's label stays grey when it fails, and so does a failed compaction's.
        for kind in ["system_notice", "compaction"] {
            let mut notice = block(BlockKind::Notice, kind, "Model changed", "");
            notice.status = "failed".into();
            let lines = render_block(&notice, 40, &context, &Extra::None);
            assert_eq!(lines[1].spans[1].style.fg, Some(theme.muted), "{kind}");
        }
    }

    #[test]
    fn a_handoff_is_prepared_again_only_when_its_item_or_a_run_it_reads_changes() {
        // Each block the next frame draws from: its item and the number it was made under.
        fn frame(open: &mut OpenThread) -> Vec<(String, u64)> {
            open.prepare();
            let prepared = &open.prepared;
            prepared
                .blocks
                .iter()
                .map(|block| block.item_id.clone())
                .zip(prepared.versions.iter().copied())
                .collect()
        }
        fn body(open: &OpenThread, index: usize) -> &str {
            &open.prepared.blocks[index].body
        }
        let snapshot = |sequence: u64, runs: Value, items: Value| {
            json!({"kind": "snapshot", "snapshotSequence": sequence, "projection": {
                "thread": {"id": "t"},
                "runs": runs,
                "turnItems": items,
            }})
        };
        let event = |sequence: u64, kind: &str, payload: &Value| {
            json!({"kind": "event", "sequence": sequence, "event": {
                "type": kind,
                "payload": payload,
            }})
        };

        // A handoff the runs name, one T3 stamped and an answer, in a thread whose runs the
        // snapshot doesn't have yet. The first handoff names both ends by their providers.
        let legacy = handoff_item("h1", 3, "completed");
        let mut stamped = handoff_item("h2", 4, "completed");
        stamped["fromModelSelections"] = json!([
            {"instanceId": "codex_personal", "model": "gpt-5.4"},
        ]);
        stamped["toModel"] = json!("claude-fable-5");
        let answer = json!({"id": "a1", "type": "assistant_message", "ordinal": 5,
            "text": "Done"});
        let mut open = OpenThread::new("t".into(), mpsc::unbounded_channel().1);
        let opened = snapshot(1, json!([]), json!([legacy, stamped, answer]));
        assert_eq!(open.apply(&opened), Applied::Snapshot);
        let first = frame(&mut open);
        assert_eq!(open.prepared.made, 3);
        assert_eq!(body(&open, 0), "codex_personal → claudeAgent");
        assert_eq!(body(&open, 1), "gpt-5.4 → claude-fable-5");
        assert_eq!(open.prepared.run_readers, HashSet::from(["h1".into()]));

        // A frame drawn for a key, the clock or a resize follows no event and makes nothing.
        for _ in 0..3 {
            assert_eq!(frame(&mut open), first);
        }
        assert_eq!(open.prepared.made, 3);

        // The runs arrive. The one before the handoff's run names the source, and the
        // handoff's run names the target. Each makes the first handoff again, and only it.
        let source = run_on("r1", 1, "completed", "codex_personal", "gpt-5.5");
        open.apply(&event(2, "run.created", &source));
        let second = frame(&mut open);
        assert_eq!(open.prepared.made, 4);
        assert_eq!((&second[1], &second[2]), (&first[1], &first[2]));
        assert_eq!(body(&open, 0), "gpt-5.5 → claudeAgent");
        let mut target = run_on("r2", 2, "running", "claudeAgent", "claude-fable-5");
        open.apply(&event(3, "run.created", &target));
        let third = frame(&mut open);
        assert_eq!(open.prepared.made, 5);
        assert_eq!((&third[1], &third[2]), (&first[1], &first[2]));
        assert_eq!(body(&open, 0), "gpt-5.5 → claude-fable-5");

        // The run finishes. Its status and times change, which no endpoint reads, so nothing is
        // made again. Nor is anything when a reconnect replays the event or the replay ends.
        target["status"] = json!("completed");
        target["completedAt"] = json!("2026-10-08T10:01:00.000Z");
        open.apply(&event(4, "run.updated", &target));
        assert_eq!(
            open.apply(&event(4, "run.updated", &target)),
            Applied::Duplicate
        );
        assert_eq!(
            open.apply(&json!({"kind": "synchronized"})),
            Applied::Synchronized
        );
        assert_eq!(frame(&mut open), third);
        assert_eq!(open.prepared.made, 5);

        // A new model on the handoff's run makes the first handoff again.
        target["modelSelection"]["model"] = json!("claude-fable-5-1");
        open.apply(&event(5, "run.updated", &target));
        let fourth = frame(&mut open);
        assert_eq!(open.prepared.made, 6);
        assert_eq!((&fourth[1], &fourth[2]), (&first[1], &first[2]));
        assert_eq!(body(&open, 0), "gpt-5.5 → claude-fable-5-1");

        // Updates to the handoff's item between two frames make its block once.
        for sequence in [6, 7, 8] {
            let mut next = legacy.clone();
            next["status"] = json!("failed");
            next["updatedAt"] = json!(format!("2026-10-08T10:02:0{sequence}.000Z"));
            open.apply(&event(sequence, "turn-item.updated", &next));
        }
        let fifth = frame(&mut open);
        assert_eq!(open.prepared.made, 7);
        assert_eq!((&fifth[1], &fifth[2]), (&first[1], &first[2]));
        assert_eq!(open.prepared.blocks[0].status, "failed");

        // A reconnect's snapshot has the first handoff stamped. Every block is made again, as
        // after any snapshot, and from then on a change to a run makes none.
        let mut now_stamped = legacy;
        now_stamped["fromModelSelections"] = json!([
            {"instanceId": "codex_personal", "model": "gpt-5.5"},
        ]);
        now_stamped["toModel"] = json!("claude-fable-5-1");
        let runs = json!([source, target]);
        let items = json!([now_stamped, stamped, answer]);
        assert_eq!(open.apply(&snapshot(9, runs, items)), Applied::Snapshot);
        let sixth = frame(&mut open);
        assert_eq!(open.prepared.made, 10);
        assert!(open.prepared.run_readers.is_empty());
        target["modelSelection"]["model"] = json!("claude-fable-6");
        open.apply(&event(10, "run.updated", &target));
        assert_eq!(frame(&mut open), sixth);
        assert_eq!(open.prepared.made, 10);
        assert_eq!(body(&open, 0), "gpt-5.5 → claude-fable-5-1");
    }

    #[test]
    fn a_run_event_makes_nothing_again_while_no_block_reads_the_runs() {
        // Each block the next frame draws from: its item and the number it was made under.
        fn frame(open: &mut OpenThread) -> Vec<(String, u64)> {
            open.prepare();
            let prepared = &open.prepared;
            prepared
                .blocks
                .iter()
                .map(|block| block.item_id.clone())
                .zip(prepared.versions.iter().copied())
                .collect()
        }
        fn bodies(open: &OpenThread) -> Vec<&str> {
            let blocks = &open.prepared.blocks;
            blocks.iter().map(|block| block.body.as_str()).collect()
        }
        // The thread's first run, before any handoff's, ran gpt-5.5 on codex_personal.
        let source = run_on("r1", 1, "completed", "codex_personal", "gpt-5.5");
        let snapshot = |items: Value| {
            json!({"kind": "snapshot", "snapshotSequence": 1, "projection": {
                "thread": {"id": "t"},
                "runs": [source],
                "turnItems": items,
            }})
        };
        let event = |sequence: u64, kind: &str, payload: &Value| {
            json!({"kind": "event", "sequence": sequence, "event": {
                "type": kind,
                "payload": payload,
            }})
        };
        let answer = json!({"id": "a1", "type": "assistant_message", "ordinal": 5,
            "text": "Done"});
        let mut stamped = handoff_item("h2", 4, "completed");
        stamped["fromModelSelections"] = json!([
            {"instanceId": "codex_personal", "model": "gpt-5.4"},
        ]);
        stamped["toModel"] = json!("claude-fable-5");

        // In a thread with no handoff, and in one whose only handoff T3 stamped at both ends,
        // no block reads the runs. The handoff's run arriving and taking a new model makes
        // nothing again, and the clock row under the prompt still follows each run event.
        let cases = [
            (json!([answer]), vec!["Done"]),
            (
                json!([stamped, answer]),
                vec!["gpt-5.4 → claude-fable-5", "Done"],
            ),
        ];
        for (items, shown) in cases {
            let mut open = OpenThread::new("t".into(), mpsc::unbounded_channel().1);
            assert_eq!(open.apply(&snapshot(items)), Applied::Snapshot);
            let first = frame(&mut open);
            assert_eq!(bodies(&open), shown);
            assert!(open.prepared.run_readers.is_empty());
            let mut target = run_on("r2", 2, "running", "claudeAgent", "claude-fable-5-1");
            open.apply(&event(2, "run.created", &target));
            target["modelSelection"]["model"] = json!("claude-fable-6");
            let update = event(3, "run.updated", &target);
            open.apply(&update);
            assert_eq!(open.apply(&update), Applied::Duplicate);
            assert!(open.prepared.changed.is_empty());
            assert_eq!(open.run_changes["r2"], 3);
            assert_eq!(frame(&mut open), first);
            assert_eq!(bodies(&open), shown);
        }

        // A handoff that reads the runs arrives after a frame, and its run arrives and takes a
        // new model before the next one. No block read the runs then, so neither run event
        // marks anything, but the next frame describes the new handoff from the runs as they
        // are by then. From that frame on it reads them, so a new model makes it again.
        let mut open = OpenThread::new("t".into(), mpsc::unbounded_channel().1);
        open.apply(&snapshot(json!([answer])));
        let first = frame(&mut open);
        let legacy = handoff_item("h1", 3, "completed");
        open.apply(&event(2, "turn-item.updated", &legacy));
        let mut target = run_on("r2", 2, "running", "claudeAgent", "claude-fable-5");
        open.apply(&event(3, "run.created", &target));
        target["modelSelection"]["model"] = json!("claude-fable-5-1");
        open.apply(&event(4, "run.updated", &target));
        assert_eq!(open.prepared.changed, HashSet::from(["h1".into()]));
        assert!(open.prepared.run_readers.is_empty());
        assert_eq!(open.run_changes["r2"], 4);
        let second = frame(&mut open);
        assert_eq!(second[1], first[0]);
        assert_eq!(bodies(&open), ["gpt-5.5 → claude-fable-5-1", "Done"]);
        assert_eq!(open.prepared.run_readers, HashSet::from(["h1".into()]));
        target["modelSelection"]["model"] = json!("claude-fable-6");
        open.apply(&event(5, "run.updated", &target));
        let third = frame(&mut open);
        assert_eq!(third[1], first[0]);
        assert_ne!(third[0], second[0]);
        assert_eq!(bodies(&open), ["gpt-5.5 → claude-fable-6", "Done"]);
    }

    // ---- hiding the sidebar ----

    const CLICK: MouseEventKind = MouseEventKind::Down(MouseButton::Left);

    /// The TUI as the event loop starts it, with a client that reaches no server. What it
    /// saves goes to `saved_in_test`, never to a settings file.
    fn tui(settings: &Settings) -> App {
        let mut app = App::new(
            Arc::new(Client::offline()),
            Theme::new(Depth::TrueColor),
            settings,
            mpsc::unbounded_channel().0,
        );
        // A thread with a worktree subscribes to its setup, which here sends nothing.
        app.watch_setup = Box::new(|_: &str| mpsc::unbounded_channel().1);
        app
    }

    /// Draws one frame of `app` on a screen `width` columns by `height` rows, as the event loop
    /// does, and returns each row as text.
    fn screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let frame = terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = frame.buffer;
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// The first screen row that shows `needle`.
    fn row_showing(rows: &[String], needle: &str) -> u16 {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("no row shows {needle:?} in {rows:#?}")) as u16
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    /// Presses each key in turn and notes where focus went after it.
    fn focus_after(app: &mut App, codes: &[KeyCode]) -> Vec<Focus> {
        let mut order = Vec::new();
        for &code in codes {
            press(app, code);
            order.push(app.focus);
        }
        order
    }

    fn ctrl_b(app: &mut App) {
        let key = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        app.on_terminal_event(Event::Key(key));
    }

    /// A mouse event at column `x` of row `y`, and whether the TUI took it.
    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) -> bool {
        app.on_terminal_event(Event::Mouse(crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }))
    }

    /// A thread as the shell lists it, in project `p`.
    fn listed(id: &str, title: &str, status: &str) -> Value {
        json!({
            "id": id,
            "projectId": "p",
            "title": title,
            "status": status,
            "latestRunId": "r",
            "lineage": {},
            "createdAt": "2026-10-01T00:00:00.000Z",
        })
    }

    /// The shell's snapshot, listing `threads`.
    fn shell_snapshot(threads: Vec<Value>) -> WatchEvent {
        let projects = json!([{"id": "p", "title": "Demo"}]);
        let snapshot = json!({"snapshotSequence": 1, "projects": projects, "threads": threads});
        WatchEvent::Item(json!({"kind": "snapshot", "snapshot": snapshot}))
    }

    /// A shell event that lists `thread` as it is now.
    fn thread_updated(sequence: u64, thread: Value) -> WatchEvent {
        let item = json!({"kind": "thread.updated", "sequence": sequence, "thread": thread});
        WatchEvent::Item(item)
    }

    /// Thread `t`, open with twenty answers long enough to wrap to a different height at each
    /// width.
    fn long_thread() -> OpenThread {
        let answers: Vec<Value> = (0..20)
            .map(|i| {
                json!({
                    "id": format!("a{i}"),
                    "type": "assistant_message",
                    "ordinal": i,
                    "text": format!("Answer {i}: {}", "word ".repeat(60)),
                })
            })
            .collect();
        let snapshot = json!({"kind": "snapshot", "snapshotSequence": 1, "projection": {
            "thread": {"id": "t"},
            "turnItems": answers,
        }});
        let mut open = OpenThread::new("t".into(), mpsc::unbounded_channel().1);
        assert_eq!(open.apply(&snapshot), Applied::Snapshot);
        open.connection = "live".into();
        open
    }

    #[test]
    fn ctrl_b_hides_the_sidebar_and_leaves_the_draft_alone() {
        let mut app = tui(&Settings::default());
        assert!(!app.sidebar_hidden, "no settings file shows it");
        assert_eq!(app.focus, Focus::Sidebar);
        let rows = screen(&mut app, 80, 24);
        assert!(rows[0].trim().is_empty(), "{rows:#?}");
        assert!(rows[1].contains("T3 Code"), "{rows:#?}");
        assert_ne!(app.sidebar.list, Rect::default());
        assert!(app.composer_area.x > app.sidebar.list.right());

        // A draft with the cursor between its two letters, then Ctrl+B.
        app.focus = Focus::Composer;
        press(&mut app, KeyCode::Char('h'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Left);
        ctrl_b(&mut app);
        assert!(app.sidebar_hidden);
        assert!(crate::settings::saved_in_test().sidebar_hidden);
        assert_eq!(app.focus, Focus::Composer);
        assert!(app.layout_changed, "the next frame waits for nothing");
        let rows = screen(&mut app, 80, 24);
        assert!(!app.layout_changed);
        assert!(!rows.iter().any(|row| row.contains("T3 Code")), "{rows:#?}");
        // The conversation takes the whole width, less a column of padding on each side.
        assert_eq!((app.composer_area.x, app.composer_area.width), (1, 78));
        assert_eq!((app.transcript_area.x, app.transcript_area.width), (1, 78));
        assert_eq!(app.sidebar_toggle, Rect::new(1, 1, 3, 1));
        assert!(rows[1].starts_with("  ◧  Pick a thread"), "{:?}", rows[1]);
        row_showing(&rows, "Press Ctrl+B to show the sidebar");
        assert!(rows[23].contains("Ctrl+B sidebar · Enter send"));
        // No `b` went in, and the cursor is still between the letters.
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.composer.text(), "hid");

        ctrl_b(&mut app);
        assert!(!app.sidebar_hidden);
        assert!(!crate::settings::saved_in_test().sidebar_hidden);
        assert_eq!(app.focus, Focus::Composer, "showing it leaves focus alone");
        let rows = screen(&mut app, 80, 24);
        assert!(rows[1].contains("T3 Code"), "{rows:#?}");
        assert!(!rows[23].contains("Ctrl+B sidebar"), "{:?}", rows[23]);
        assert_eq!(app.composer.text(), "hid");
    }

    #[test]
    fn focus_never_rests_on_a_hidden_sidebar() {
        use super::Focus::{Composer, Sidebar, Transcript};
        use crossterm::event::KeyCode::{BackTab, Tab};

        // Saved hidden, it starts hidden, with focus on the composer. Tab and BackTab skip it.
        let hidden = Settings {
            sidebar_hidden: true,
            ..Settings::default()
        };
        let mut app = tui(&hidden);
        assert!(app.sidebar_hidden);
        assert_eq!(app.focus, Composer);
        let order = focus_after(&mut app, &[Tab, Tab, BackTab, BackTab]);
        assert_eq!(order, [Transcript, Composer, Transcript, Composer]);

        // Shown, it is back in the cycle.
        ctrl_b(&mut app);
        let order = focus_after(&mut app, &[Tab, Tab, Tab, BackTab]);
        assert_eq!(order, [Transcript, Sidebar, Composer, Sidebar]);

        // Hiding it while it has focus moves focus to the composer with no thread open, and to
        // the transcript with one.
        ctrl_b(&mut app);
        assert_eq!(app.focus, Composer);
        ctrl_b(&mut app);
        app.open = Some(long_thread());
        app.focus = Sidebar;
        ctrl_b(&mut app);
        assert_eq!(app.focus, Transcript);

        // From the transcript, the keys that go to the sidebar show it first.
        for code in [KeyCode::Esc, KeyCode::Left, KeyCode::Char('h')] {
            press(&mut app, code);
            assert!(!app.sidebar_hidden, "{code:?}");
            assert!(!crate::settings::saved_in_test().sidebar_hidden);
            assert_eq!(app.focus, Sidebar);
            ctrl_b(&mut app);
            assert_eq!(app.focus, Transcript);
        }
    }

    #[test]
    fn a_hidden_sidebar_leaves_its_clicks_and_wheel_to_the_conversation() {
        let mut app = tui(&Settings::default());
        // Two finished runs with no visit, which counts as seen, so both cards are in the inbox
        // at the top of the list, and the empty Working, Done and Settled shelves sit at its
        // bottom. A thread whose status is idle after a run reads Waiting and belongs in
        // Working, which would put the cards at the bottom, over the composer once the sidebar
        // hides.
        let threads = vec![
            listed("a", "Alpha", "completed"),
            listed("b", "Bravo", "completed"),
        ];
        app.on_shell_event(shell_snapshot(threads));
        app.open = Some(long_thread());
        assert!(app.sidebar.select("a"));
        let rows = screen(&mut app, 80, 24);
        // Shown, a click on Bravo's card would open it, and the wheel there moves the highlight.
        let (x, y) = (5, row_showing(&rows, "Bravo"));
        assert_eq!(app.sidebar.thread_at(y), Some("b"));
        // The Settled heading ends the list, on the screen's last row.
        let settled = row_showing(&rows, "Settled (0)");
        assert_eq!(settled, 23);
        assert_eq!(app.sidebar.shelf_at(settled), Some(Shelf::Settled));
        let working = row_showing(&rows, "Working (0)");
        assert_eq!(app.sidebar.shelf_at(working), Some(Shelf::Working));

        app.focus = Focus::Composer;
        ctrl_b(&mut app);
        let hidden = screen(&mut app, 80, 24);
        // Hidden, no row takes a click as a card or a heading.
        for row in 0..24 {
            assert_eq!(app.sidebar.thread_at(row), None, "row {row}");
            assert_eq!(app.sidebar.shelf_at(row), None, "row {row}");
        }
        // Bravo's old place is inside the transcript, and the Settled heading's is the status
        // line.
        let body = app.transcript_area;
        assert!(
            (body.x..body.right()).contains(&x) && (body.y..body.bottom()).contains(&y),
            "({x}, {y}) is outside {body:?}"
        );
        assert!(
            hidden[settled as usize].contains("Ctrl+B sidebar"),
            "{hidden:#?}"
        );
        // Hidden, the wheel there scrolls the transcript and the highlight stays on Alpha.
        assert!(mouse(&mut app, MouseEventKind::ScrollUp, x, y));
        assert_eq!(app.scroll.rows(), 3);
        assert_eq!(app.sidebar.selected_thread_id(), Some("a"));
        // A click there focuses the transcript and opens nothing.
        assert!(mouse(&mut app, CLICK, x, y));
        assert_eq!(app.focus, Focus::Transcript);
        assert_eq!(app.open.as_ref().map(|open| open.id.as_str()), Some("t"));
        // A click where the Settled heading was lands on the status line and opens nothing.
        assert!(mouse(&mut app, CLICK, x, settled));
        assert_eq!(app.focus, Focus::Composer);
        assert!(!crate::settings::saved_in_test().sidebar_settled_shelf_expanded);

        // The toggle brings the sidebar back as it was, and its cards take clicks again.
        let toggle = app.sidebar_toggle;
        assert!(mouse(&mut app, CLICK, toggle.x, toggle.y));
        assert!(!app.sidebar_hidden);
        assert!(!crate::settings::saved_in_test().sidebar_hidden);
        let rows = screen(&mut app, 80, 24);
        let bravo = row_showing(&rows, "Bravo");
        assert_eq!(app.sidebar.thread_at(bravo), Some("b"));
        assert_eq!(app.sidebar.selected_thread_id(), Some("a"));
        // The toggle moved right of the sidebar, and a click on it there hides it again.
        let toggle = app.sidebar_toggle;
        assert!(toggle.x > app.sidebar.list.right());
        assert!(mouse(&mut app, CLICK, toggle.x + 2, toggle.y));
        assert!(app.sidebar_hidden);
        assert!(crate::settings::saved_in_test().sidebar_hidden);
    }

    #[test]
    fn w_d_and_e_open_and_close_the_shelves_from_the_sidebar_and_remember_it() {
        let mut app = tui(&Settings::default());
        let mut delta = listed("d", "Delta", "completed");
        delta["latestRunCompletedAt"] = json!("2026-10-08T11:00:00.000Z");
        delta["lastVisitedAt"] = json!("2026-10-08T10:00:00.000Z");
        let threads = vec![listed("w", "Whiskey", "running"), delta];
        app.on_shell_event(shell_snapshot(threads));
        assert_eq!(app.focus, Focus::Sidebar);
        // A new settings file has Working and Done open and Settled closed, and the status
        // line offers the keys.
        let rows = screen(&mut app, 200, 30);
        for shown in [
            "Working (1) ",
            "Whiskey",
            "Done (1) ",
            "Delta",
            "Settled (0) ",
        ] {
            row_showing(&rows, shown);
        }
        assert!(
            rows[29].contains("w working · d done · e settled"),
            "{}",
            rows[29]
        );

        // Each key opens or closes its own shelf and saves the choice.
        press(&mut app, KeyCode::Char('w'));
        assert!(!crate::settings::saved_in_test().sidebar_working_shelf_expanded);
        press(&mut app, KeyCode::Char('d'));
        assert!(!crate::settings::saved_in_test().sidebar_done_shelf_expanded);
        press(&mut app, KeyCode::Char('e'));
        assert!(crate::settings::saved_in_test().sidebar_settled_shelf_expanded);
        let rows = screen(&mut app, 200, 30);
        assert!(
            !shows(&rows, "Whiskey") && !shows(&rows, "Delta"),
            "{rows:#?}"
        );
        let at = |label: &str| rows[row_showing(&rows, label) as usize].clone();
        assert!(at("Working (1) ").contains("─ ›"));
        assert!(at("Done (1) ").contains("─ ›"));
        assert!(at("Settled (0) ").contains("─ ⌄"));

        // A click on a heading does the same.
        let working = row_showing(&rows, "Working (1) ");
        assert_eq!(app.sidebar.shelf_at(working), Some(Shelf::Working));
        assert!(mouse(&mut app, CLICK, 5, working));
        assert!(crate::settings::saved_in_test().sidebar_working_shelf_expanded);
        assert_eq!(app.focus, Focus::Sidebar);
        let rows = screen(&mut app, 200, 30);
        assert_eq!(
            app.sidebar.thread_at(row_showing(&rows, "Whiskey")),
            Some("w")
        );

        // In the composer the same letters are text, and the shelves stay as they are.
        app.focus = Focus::Composer;
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.composer.text(), "d");
        assert!(!crate::settings::saved_in_test().sidebar_done_shelf_expanded);
        assert!(!shows(&screen(&mut app, 200, 30), "Delta"));
    }

    #[test]
    fn a_fold_draws_at_once_and_its_old_rows_take_no_click_until_then() {
        let mut app = tui(&Settings::default());
        // Five inbox cards fill the list down to Working, so Done waits below the screen
        // until Working closes and Done takes Whiskey's place.
        let mut threads: Vec<Value> = (1..=5)
            .map(|n| listed(&format!("a{n}"), &format!("Inbox {n}"), "completed"))
            .collect();
        let mut delta = listed("d", "Delta", "completed");
        delta["latestRunCompletedAt"] = json!("2026-10-08T11:00:00.000Z");
        delta["lastVisitedAt"] = json!("2026-10-08T10:00:00.000Z");
        threads.extend([listed("w", "Whiskey", "running"), delta]);
        app.on_shell_event(shell_snapshot(threads));
        app.open = Some(long_thread());
        app.composer.insert_str("half a thought");
        assert_eq!(app.focus, Focus::Sidebar);
        let rows = screen(&mut app, 200, 30);
        let working = row_showing(&rows, "Working (1) ");
        let whiskey = row_showing(&rows, "Whiskey");
        assert_eq!(app.sidebar.shelf_at(working), Some(Shelf::Working));
        assert_eq!(app.sidebar.thread_at(whiskey), Some("w"));
        assert!(!shows(&rows, "Done ("), "{rows:#?}");
        app.scroll.set(3);
        let still = |app: &App| {
            assert_eq!(app.focus, Focus::Sidebar);
            assert_eq!(app.open.as_ref().map(|open| open.id.as_str()), Some("t"));
            assert_eq!(app.composer.text(), "half a thought");
        };

        // `w` closes Working, and the next frame waits for nothing. Until it draws, a click
        // where Whiskey's card or the Working heading was opens nothing and folds nothing.
        press(&mut app, KeyCode::Char('w'));
        assert!(app.layout_changed);
        assert!(!crate::settings::saved_in_test().sidebar_working_shelf_expanded);
        for row in 0..30 {
            assert_eq!(app.sidebar.thread_at(row), None, "row {row}");
            assert_eq!(app.sidebar.shelf_at(row), None, "row {row}");
        }
        for row in [working, whiskey - 1, whiskey] {
            assert!(mouse(&mut app, CLICK, 5, row));
        }
        assert!(!crate::settings::saved_in_test().sidebar_working_shelf_expanded);
        assert!(crate::settings::saved_in_test().sidebar_done_shelf_expanded);
        assert_eq!(app.scroll.rows(), 3);
        still(&app);

        // The frame draws Done where Whiskey was, and those rows now take Done's clicks.
        let rows = screen(&mut app, 200, 30);
        assert!(!app.layout_changed);
        assert!(!shows(&rows, "Whiskey"), "{rows:#?}");
        let done = row_showing(&rows, "Done (1) ");
        let delta = row_showing(&rows, "Delta");
        assert_eq!(done, whiskey - 1);
        assert_eq!(app.sidebar.shelf_at(working), Some(Shelf::Working));
        assert_eq!(app.sidebar.shelf_at(done), Some(Shelf::Done));
        assert_eq!(app.sidebar.thread_at(delta), Some("d"));

        // A click on Done closes it, and a second click on the same row before the next
        // frame does nothing, nor does one on Delta's old card.
        assert!(mouse(&mut app, CLICK, 5, done));
        assert!(app.layout_changed);
        assert!(!crate::settings::saved_in_test().sidebar_done_shelf_expanded);
        assert!(mouse(&mut app, CLICK, 5, done));
        assert!(mouse(&mut app, CLICK, 5, delta));
        assert!(!crate::settings::saved_in_test().sidebar_done_shelf_expanded);
        assert_eq!(app.scroll.rows(), 3);
        still(&app);
        let rows = screen(&mut app, 200, 30);
        assert!(!shows(&rows, "Delta"), "{rows:#?}");
        let done = row_showing(&rows, "Done (1) ");
        assert!(rows[done as usize].contains("─ ›"), "{rows:#?}");
        assert_eq!(app.sidebar.shelf_at(done), Some(Shelf::Done));
        still(&app);
    }

    /// Settings with Working, Done and Settled closed.
    fn shelves_closed() -> Settings {
        Settings {
            sidebar_working_shelf_expanded: false,
            sidebar_done_shelf_expanded: false,
            sidebar_settled_shelf_expanded: false,
            ..Settings::default()
        }
    }

    /// A thread whose latest run finished after the last visit, which puts it on Done.
    fn finished_unseen(id: &str, title: &str) -> Value {
        let mut thread = listed(id, title, "completed");
        thread["latestRunCompletedAt"] = json!("2026-10-08T11:00:00.000Z");
        thread["lastVisitedAt"] = json!("2026-10-08T10:00:00.000Z");
        thread
    }

    /// Inbox cards a1 to a4 over the closed shelves, with Whiskey on Working and Delta on Done,
    /// and the settings saved as the TUI starts with them. At 80x8 the list has six rows: room
    /// for a4 and the Working heading under it, but not for Done and Settled too.
    fn short_list_over_closed_shelves() -> App {
        Settings::update(|saved| *saved = shelves_closed());
        let mut app = tui(&shelves_closed());
        let mut threads: Vec<Value> = (1..=4)
            .map(|n| listed(&format!("a{n}"), &format!("Inbox {n}"), "completed"))
            .collect();
        threads.extend([
            listed("w", "Whiskey", "running"),
            finished_unseen("d", "Delta"),
        ]);
        app.on_shell_event(shell_snapshot(threads));
        app.open = Some(long_thread());
        app
    }

    /// What a click on each row of the sidebar's list lands on, by the last frame: a thread's
    /// id, a shelf's name or nothing.
    fn clickable(app: &App) -> Vec<String> {
        let list = app.sidebar.list;
        (list.y..list.bottom())
            .map(
                |y| match (app.sidebar.thread_at(y), app.sidebar.shelf_at(y)) {
                    (Some(id), _) => id.to_string(),
                    (None, Some(shelf)) => format!("{shelf:?}"),
                    (None, None) => String::new(),
                },
            )
            .collect()
    }

    #[test]
    fn down_on_the_last_card_of_a_short_list_scrolls_on_to_the_headings_under_it() {
        let mut app = short_list_over_closed_shelves();
        let rows = screen(&mut app, 80, 8);
        assert_eq!((app.sidebar.list.y, app.sidebar.list.bottom()), (2, 8));
        assert_eq!(clickable(&app), ["a1", "a1", "a1", "", "a2", "a2"]);
        assert!(!shows(&rows, "Working ("), "{rows:#?}");
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
            screen(&mut app, 80, 8);
        }
        // a4, the last card, fills the list with Working under it. Done and Settled don't fit.
        assert_eq!(app.sidebar.selected_thread_id(), Some("a4"));
        let at_a4 = ["a4", "a4", "a4", "", "", "Working"];
        let rows = screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), at_a4);
        assert!(rows[7].contains("Working (1) ─"), "{rows:#?}");
        assert!(!shows(&rows, "Done ("), "{rows:#?}");

        // Down scrolls on past a4, and the first frame after shows every heading under it, each
        // taking clicks on the row that reads it. The highlight stays on a4.
        let past_a4 = ["", "Working", "Done", "Settled", "", ""];
        press(&mut app, KeyCode::Down);
        let rows = screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), past_a4);
        assert!(rows[3].contains("Working (1) ─"), "{rows:#?}");
        assert!(rows[4].contains("Done (1) ─"), "{rows:#?}");
        assert!(rows[5].contains("Settled (0) ─"), "{rows:#?}");
        assert_eq!(app.sidebar.selected_thread_id(), Some("a4"));

        // That is as far as it goes. Repaints and a live change near the bottom keep it there.
        press(&mut app, KeyCode::Down);
        screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), past_a4);
        app.on_shell_event(thread_updated(2, finished_unseen("e", "Echo")));
        let rows = screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), past_a4);
        assert!(rows[4].contains("Done (2) ─"), "{rows:#?}");
        screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), past_a4);
        assert_eq!(app.sidebar.selected_thread_id(), Some("a4"));

        // Up scrolls back to a4 first, and then moves the highlight.
        press(&mut app, KeyCode::Up);
        screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), at_a4);
        press(&mut app, KeyCode::Up);
        screen(&mut app, 80, 8);
        assert_eq!(app.sidebar.selected_thread_id(), Some("a3"));
        assert_eq!(clickable(&app), ["a3", "a3", "a3", "", "a4", "a4"]);

        // Enter on the list scrolled past a4 opens a4, and the next frame shows it again.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), past_a4);
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.open.as_ref().map(|open| open.id.as_str()), Some("a4"));
        screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), at_a4);
    }

    #[test]
    fn the_wheel_scrolls_a_short_list_on_to_its_headings_and_their_clicks_fold_them() {
        let mut app = short_list_over_closed_shelves();
        screen(&mut app, 80, 8);
        // Three turns take the highlight to a4, the fourth scrolls past it, and more do nothing.
        for _ in 0..6 {
            assert!(mouse(&mut app, MouseEventKind::ScrollDown, 5, 4));
            screen(&mut app, 80, 8);
        }
        assert_eq!(clickable(&app), ["", "Working", "Done", "Settled", "", ""]);
        assert_eq!(app.sidebar.selected_thread_id(), Some("a4"));

        // A click on Done opens it. Nothing the last frame drew takes a click until the next
        // one draws, so a click where Settled was folds nothing.
        assert!(mouse(&mut app, CLICK, 5, 4));
        assert!(app.layout_changed);
        assert!(crate::settings::saved_in_test().sidebar_done_shelf_expanded);
        assert!(clickable(&app).iter().all(String::is_empty));
        assert!(mouse(&mut app, CLICK, 5, 5));
        assert!(!crate::settings::saved_in_test().sidebar_settled_shelf_expanded);

        // The frame keeps the view where it was, with Delta under the open Done.
        let rows = screen(&mut app, 80, 8);
        assert!(!app.layout_changed);
        assert_eq!(clickable(&app), ["", "Working", "Done", "d", "d", "d"]);
        assert!(rows[4].contains("Done (1) ─"), "{rows:#?}");
        assert!(rows[4].contains("─ ⌄"), "{rows:#?}");
        assert!(shows(&rows, "Delta"), "{rows:#?}");

        // A click on Delta opens it, and the list follows the highlight to it, with Settled
        // under it.
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        assert!(mouse(&mut app, CLICK, 5, 6));
        assert_eq!(app.open.as_ref().map(|open| open.id.as_str()), Some("d"));
        screen(&mut app, 80, 8);
        assert_eq!(app.sidebar.selected_thread_id(), Some("d"));
        assert_eq!(clickable(&app), ["d", "d", "d", "", "", "Settled"]);

        // A click there opens Settled, and the wheel goes back up through a4 to a3.
        assert!(mouse(&mut app, CLICK, 5, 7));
        assert!(crate::settings::saved_in_test().sidebar_settled_shelf_expanded);
        let rows = screen(&mut app, 80, 8);
        assert!(rows[7].contains("Settled (0) ─"), "{rows:#?}");
        assert!(rows[7].contains("─ ⌄"), "{rows:#?}");
        assert!(mouse(&mut app, MouseEventKind::ScrollUp, 5, 4));
        screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), ["a4", "a4", "a4", "", "", "Working"]);
        assert!(mouse(&mut app, MouseEventKind::ScrollUp, 5, 4));
        screen(&mut app, 80, 8);
        assert_eq!(app.sidebar.selected_thread_id(), Some("a3"));
    }

    #[test]
    fn a_list_with_room_for_every_row_has_nothing_to_scroll_past() {
        let mut app = short_list_over_closed_shelves();
        // At 120x36 the whole list fits, with the shelves at its bottom.
        let rows = screen(&mut app, 120, 36);
        assert_eq!(row_showing(&rows, "Working (1)"), 33);
        assert_eq!(row_showing(&rows, "Settled (0)"), 35);
        assert_eq!(app.sidebar.shelf_at(35), Some(Shelf::Settled));
        let tall = clickable(&app);
        // Down past a4 scrolls nothing, so Up moves the highlight at once.
        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
            screen(&mut app, 120, 36);
            assert_eq!(clickable(&app), tall);
        }
        assert_eq!(app.sidebar.selected_thread_id(), Some("a4"));
        press(&mut app, KeyCode::Up);
        assert_eq!(app.sidebar.selected_thread_id(), Some("a3"));

        // Shrunk to 80x8, Down scrolls past a4. Grown back, the list fits and shows as before.
        // Shrunk again, it follows a4, so Up moves the highlight at once.
        press(&mut app, KeyCode::Down);
        app.on_terminal_event(Event::Resize(80, 8));
        screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), ["a4", "a4", "a4", "", "", "Working"]);
        press(&mut app, KeyCode::Down);
        screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), ["", "Working", "Done", "Settled", "", ""]);
        app.on_terminal_event(Event::Resize(120, 36));
        let rows = screen(&mut app, 120, 36);
        assert_eq!(clickable(&app), tall);
        assert_eq!(row_showing(&rows, "Settled (0)"), 35);
        app.on_terminal_event(Event::Resize(80, 8));
        screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), ["a4", "a4", "a4", "", "", "Working"]);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.sidebar.selected_thread_id(), Some("a3"));
    }

    #[test]
    fn a_short_list_of_headings_scrolls_to_the_last_and_an_empty_list_stays_still() {
        // With no thread, there is nothing to scroll to and nothing takes a click.
        let mut app = tui(&shelves_closed());
        app.on_shell_event(shell_snapshot(Vec::new()));
        screen(&mut app, 80, 8);
        press(&mut app, KeyCode::Down);
        assert!(mouse(&mut app, MouseEventKind::ScrollDown, 5, 4));
        let rows = screen(&mut app, 80, 8);
        assert!(
            rows[2].trim_start().starts_with("No threads yet"),
            "{rows:#?}"
        );
        assert!(clickable(&app).iter().all(String::is_empty));
        press(&mut app, KeyCode::Up);
        let rows = screen(&mut app, 80, 8);
        assert!(
            rows[2].trim_start().starts_with("No threads yet"),
            "{rows:#?}"
        );

        // Delta alone, on closed Done, leaves only the headings. At 80x4 the list has two rows,
        // and Down scrolls on to Settled, whose click opens it.
        let mut app = tui(&shelves_closed());
        app.on_shell_event(shell_snapshot(vec![finished_unseen("d", "Delta")]));
        screen(&mut app, 80, 4);
        assert_eq!(clickable(&app), ["Working", "Done"]);
        for _ in 0..2 {
            press(&mut app, KeyCode::Down);
            screen(&mut app, 80, 4);
            assert_eq!(clickable(&app), ["Done", "Settled"]);
        }
        assert!(mouse(&mut app, CLICK, 5, 3));
        assert!(crate::settings::saved_in_test().sidebar_settled_shelf_expanded);
        let rows = screen(&mut app, 80, 4);
        assert_eq!(clickable(&app), ["Done", "Settled"]);
        assert!(rows[3].contains("Settled (0) ─"), "{rows:#?}");
        assert!(rows[3].contains("─ ⌄"), "{rows:#?}");
        press(&mut app, KeyCode::Up);
        screen(&mut app, 80, 4);
        assert_eq!(clickable(&app), ["Working", "Done"]);
    }

    #[test]
    fn the_open_thread_on_a_closed_shelf_stays_in_reach_above_the_headings_under_it() {
        let mut app = short_list_over_closed_shelves();
        // Whiskey is open, so closed Working still lists it.
        app.open = Some(OpenThread::new("w".into(), mpsc::unbounded_channel().1));
        app.rebuild_rows();
        screen(&mut app, 80, 8);
        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
            screen(&mut app, 80, 8);
        }
        assert_eq!(app.sidebar.selected_thread_id(), Some("w"));
        let at_whiskey = ["", "Working", "w", "w", "w", ""];
        assert_eq!(clickable(&app), at_whiskey);

        // Down scrolls past Whiskey to Done and Settled, and no further.
        let past_whiskey = ["", "Done", "Settled", "", "", ""];
        for _ in 0..2 {
            press(&mut app, KeyCode::Down);
            screen(&mut app, 80, 8);
            assert_eq!(clickable(&app), past_whiskey);
        }

        // Opening Working there keeps the view. Closing it moves the highlight off its cards to
        // Whiskey, which stays listed, and the list follows the highlight again.
        press(&mut app, KeyCode::Char('w'));
        screen(&mut app, 80, 8);
        assert_eq!(clickable(&app), past_whiskey);
        press(&mut app, KeyCode::Char('w'));
        let rows = screen(&mut app, 80, 8);
        assert_eq!(app.sidebar.selected_thread_id(), Some("w"));
        assert_eq!(clickable(&app), at_whiskey);
        assert!(rows[3].contains("Working (1) ─"), "{rows:#?}");
        assert!(rows[3].contains("─ ›"), "{rows:#?}");
    }

    #[test]
    fn a_tall_screen_keeps_a_blank_row_at_the_top_and_a_short_one_uses_it() {
        let mut app = tui(&Settings::default());
        let threads = vec![listed("a", "Alpha", "idle"), listed("b", "Bravo", "idle")];
        app.on_shell_event(shell_snapshot(threads));
        app.open = Some(long_thread());
        for (width, height) in [(120, 36), (80, 24), (40, 12), (10, 5)] {
            let rows = screen(&mut app, width, height);
            let top = u16::from(height >= 20);
            // The toggle leads the title row, under the blank row when there is one.
            assert_eq!(app.sidebar_toggle.y, top, "{width}x{height}");
            if top == 1 {
                assert!(rows[0].trim().is_empty(), "{width}x{height}: {rows:#?}");
            }
            let sidebar = &app.sidebar;
            if sidebar.list == Rect::default() {
                // Too narrow for the sidebar, so nothing of it takes a click.
                assert_eq!(width, 10);
            } else {
                // The wordmark, the environment, then the list after another blank row when
                // there is room, running to the bottom.
                assert!(rows[top as usize].contains("T3 Code"), "{rows:#?}");
                assert_eq!(sidebar.list.y, 2 + 2 * top, "{width}x{height}");
                assert_eq!(sidebar.list.bottom(), height);
            }
            // Every card and heading drawn takes clicks inside the list and nowhere else.
            for y in 0..height {
                let hit = sidebar.thread_at(y).is_some() || sidebar.shelf_at(y).is_some();
                let inside = (sidebar.list.y..sidebar.list.bottom()).contains(&y);
                assert!(!hit || inside, "{width}x{height} row {y}");
            }
        }
    }

    #[test]
    fn chips_and_an_open_menu_follow_the_composer_when_the_sidebar_hides() {
        let mut app = tui(&Settings::default());
        app.open = Some(long_thread());
        app.focus = Focus::Composer;
        app.picker = Some(Picker {
            kind: Kind::Mode,
            filter: String::new(),
            selected: 0,
            offset: 0,
        });
        screen(&mut app, 80, 24);
        let (chips, menu) = (app.chips.clone(), app.picker_area);
        assert!(!chips.is_empty());
        assert_ne!(menu, Rect::default());

        // Ctrl+B leaves the menu open, and the next frame lays both out in the wider composer.
        ctrl_b(&mut app);
        assert!(app.picker.is_some());
        screen(&mut app, 80, 24);
        let composer = app.composer_area;
        assert_eq!(app.chips.len(), chips.len());
        for ((area, kind), (before, kind_before)) in app.chips.iter().zip(&chips) {
            assert_eq!(kind, kind_before);
            assert!(area.x < before.x, "{kind:?} stayed at {before:?}");
            assert!(area.x > composer.x && area.right() < composer.right());
        }
        assert!(app.picker_area.x < menu.x);
        assert!(app.picker_area.x >= composer.x);
        assert!(app.picker_area.right() <= composer.right());
    }

    #[test]
    fn hiding_the_sidebar_rewraps_the_transcript_and_keeps_the_reader_in_place() {
        let mut app = tui(&Settings::default());
        app.open = Some(long_thread());
        app.composer.insert_str("draft");
        app.focus = Focus::Transcript;
        screen(&mut app, 80, 24);
        for _ in 0..3 {
            press(&mut app, KeyCode::PageUp);
        }
        screen(&mut app, 80, 24);
        let narrow = app.transcript_area.width - 1;
        let (top, _) = app.anchor.clone().expect("scrolled up");
        let prepared = &app.open.as_ref().unwrap().prepared;
        let (made, versions) = (prepared.made, prepared.versions.clone());

        for hidden in [true, false] {
            ctrl_b(&mut app);
            assert_eq!(app.sidebar_hidden, hidden);
            assert_eq!(app.focus, Focus::Transcript, "the transcript keeps focus");
            screen(&mut app, 80, 24);
            let width = app.transcript_area.width - 1;
            assert_eq!(width > narrow, hidden);
            // Every block is wrapped again at the new width, from the blocks already made.
            assert!(app.cache.values().all(|cached| cached.key.1 == width));
            let prepared = &app.open.as_ref().unwrap().prepared;
            assert_eq!(prepared.made, made, "a block was made again");
            assert_eq!(prepared.versions, versions);
            // The same answer is at the top, and the transcript is still scrolled up.
            assert_eq!(app.anchor.as_ref().map(|(id, _)| id), Some(&top));
            assert!(app.scroll.rows() > 0);
            assert_eq!(app.composer.text(), "draft");
        }
    }

    #[test]
    fn the_sidebar_comes_back_with_what_changed_while_it_was_hidden() {
        let mut app = tui(&Settings::default());
        let threads = vec![listed("a", "Alpha", "idle"), listed("b", "Bravo", "idle")];
        app.on_shell_event(shell_snapshot(threads));
        assert!(app.sidebar.select("b"));
        ctrl_b(&mut app);
        let rows = screen(&mut app, 80, 24);
        assert!(!rows.iter().any(|row| row.contains("Bravo")), "{rows:#?}");

        // T3 renames Bravo and lists a new thread while the sidebar is hidden.
        app.on_shell_event(thread_updated(2, listed("b", "Bravo renamed", "idle")));
        app.on_shell_event(thread_updated(3, listed("c", "Charlie", "idle")));
        screen(&mut app, 80, 24);
        ctrl_b(&mut app);
        let rows = screen(&mut app, 80, 24);
        let bravo = row_showing(&rows, "Bravo renamed");
        let charlie = row_showing(&rows, "Charlie");
        assert_eq!(app.sidebar.thread_at(bravo), Some("b"));
        assert_eq!(app.sidebar.thread_at(charlie), Some("c"));
        assert_eq!(app.sidebar.selected_thread_id(), Some("b"));
    }

    #[test]
    fn hidden_working_cards_need_no_tick() {
        let mut app = tui(&Settings::default());
        app.on_shell_event(shell_snapshot(vec![listed("w", "Busy", "running")]));
        screen(&mut app, 80, 24);
        assert!(needs_tick(app.open.as_ref(), &app.sidebar));
        ctrl_b(&mut app);
        screen(&mut app, 80, 24);
        assert!(!needs_tick(app.open.as_ref(), &app.sidebar));
        ctrl_b(&mut app);
        screen(&mut app, 80, 24);
        assert!(needs_tick(app.open.as_ref(), &app.sidebar));
    }

    #[test]
    fn a_hidden_sidebar_still_wakes_a_snoozed_thread() {
        let mut client = Client::offline();
        client.runtime.capabilities = json!({"threadSnooze": true});
        let mut app = App::new(
            Arc::new(client),
            Theme::new(Depth::TrueColor),
            &Settings::default(),
            mpsc::unbounded_channel().0,
        );
        let mut zulu = listed("z", "Zulu", "idle");
        zulu["snoozedUntil"] = json!("2026-01-01T00:00:00.000Z");
        app.on_shell_event(shell_snapshot(vec![zulu]));
        // Laid out the day before its snooze ended, as by a timer that slept with the machine.
        let threads = app.shell.as_ref().unwrap().threads.clone();
        let capabilities = app.capabilities();
        let before = parse_iso_ms("2025-12-31T00:00:00.000Z").unwrap();
        app.sidebar.rebuild(&threads, capabilities, None, before);
        let wake = parse_iso_ms("2026-01-01T00:00:00.000Z");
        assert_eq!(app.sidebar.next_wake, wake);

        // A hidden frame still moves it back. A wake left in the past would fire at once and
        // again on every pass of the event loop.
        ctrl_b(&mut app);
        screen(&mut app, 80, 24);
        assert_eq!(app.sidebar.next_wake, None);
        ctrl_b(&mut app);
        let rows = screen(&mut app, 80, 24);
        assert!(row_showing(&rows, "Working (1)") < row_showing(&rows, "Zulu"));
        assert!(!rows.iter().any(|row| row.contains("Snoozed")), "{rows:#?}");
    }

    #[test]
    fn any_screen_size_draws_with_the_sidebar_shown_or_hidden() {
        let starts = [
            (false, None),
            (true, None),
            (false, Some(0)),
            (false, Some(45)),
            (true, Some(u16::MAX)),
        ];
        for (start_hidden, chosen) in starts {
            let settings = Settings {
                sidebar_hidden: start_hidden,
                sidebar_width: chosen,
                ..Settings::default()
            };
            let mut app = tui(&settings);
            let threads = vec![listed("a", "Ann", "running"), listed("b", "Bo", "idle")];
            app.on_shell_event(shell_snapshot(threads));
            app.open = Some(long_thread());
            app.scroll.set(30);
            app.composer.insert_str("a draft that wraps when narrow");
            app.picker = Some(Picker {
                kind: Kind::Mode,
                filter: String::new(),
                selected: 0,
                offset: 0,
            });
            let small = [(0, 0), (1, 1), (2, 2), (10, 5), (19, 3), (26, 4)];
            // Growing, then shrinking, so nothing from a larger frame is left behind.
            for (width, height) in small.into_iter().chain([(80, 24), (200, 60), (26, 4)]) {
                let whole = Rect::new(0, 0, width, height);
                for _ in 0..2 {
                    screen(&mut app, width, height);
                    let drawn = [
                        app.sidebar_toggle,
                        app.transcript_area,
                        app.composer_area,
                        app.picker_area,
                        app.sidebar.list,
                        app.sidebar.border,
                    ];
                    for area in drawn {
                        assert_eq!(area.intersection(whole), area, "{width}x{height}");
                    }
                    if app.sidebar_hidden {
                        assert_eq!(app.sidebar.list, Rect::default());
                        assert_eq!(app.sidebar.border, Rect::default());
                        assert!(!app.sidebar.drew_working());
                    } else if app.sidebar.border != Rect::default() {
                        // The edge is the sidebar's last column. The conversation takes the
                        // rest, starting with its column of padding.
                        let sidebar = app.sidebar.border.right();
                        assert_eq!(app.composer_area.x, sidebar + 1, "{width} {chosen:?}");
                        assert!(width - sidebar >= NARROWEST_MAIN, "{width}x{height}");
                    }
                    ctrl_b(&mut app);
                }
            }
        }
    }

    // ---- resizing the sidebar ----

    /// An open mode menu, at its first item.
    fn mode_menu() -> Picker {
        Picker {
            kind: Kind::Mode,
            filter: String::new(),
            selected: 0,
            offset: 0,
        }
    }

    /// Presses the left button on the sidebar's edge and drags it to column `to`, then draws
    /// as the event loop would. The button stays down.
    fn drag_edge(app: &mut App, to: u16) {
        let Rect { width, height, .. } = app.screen;
        let edge = app.sidebar.border;
        mouse(app, CLICK, edge.x, edge.y + 5);
        assert!(app.drag.is_some(), "no drag started at {edge:?}");
        mouse(app, MouseEventKind::Drag(MouseButton::Left), to, edge.y + 5);
        screen(app, width, height);
        assert_eq!(app.sidebar.border.x, to);
    }

    #[test]
    fn with_no_width_chosen_the_sidebar_is_as_wide_as_before() {
        // Before a width could be chosen, ratatui's layout solver split the screen. With none
        // chosen, the sidebar gets the same columns at every width, the tiny ones included.
        for width in 0..=300 {
            let area = Rect::new(0, 0, width, 24);
            let old = (width / 4).clamp(26, 34).min(width / 2);
            let widths = [Constraint::Length(old), Constraint::Min(20)];
            let [sidebar, main] = Layout::horizontal(widths).areas(area);
            let drawn = columns(area, sidebar_width(width, None));
            assert_eq!(drawn, (sidebar, main), "{width}");
        }
        // Hidden, the sidebar has no width and the conversation has the screen.
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(columns(area, 0), (Rect { width: 0, ..area }, area));
    }

    #[test]
    fn a_chosen_width_draws_where_the_screen_has_room_and_comes_back_when_it_grows() {
        assert_eq!(sidebar_width(120, Some(45)), 45);
        // No narrower than 20 columns and no wider than half the screen.
        assert_eq!(sidebar_width(120, Some(0)), 20);
        assert_eq!(sidebar_width(80, Some(45)), 40);
        assert_eq!(sidebar_width(120, Some(u16::MAX)), 60);
        // A screen too narrow for both leaves the conversation its 20 columns first.
        assert_eq!(sidebar_width(30, Some(45)), 10);
        assert_eq!(sidebar_width(30, Some(0)), 10);
        assert_eq!(sidebar_width(0, Some(45)), 0);
        assert_eq!(sidebar_width(u16::MAX, Some(u16::MAX)), u16::MAX / 2);

        // Saved at 50, it draws at 50 on a wide screen and at 40 on an 80-column one, and the
        // narrow screen doesn't change the 50, so it comes back when the screen grows.
        let chosen = Settings {
            sidebar_width: Some(50),
            ..Settings::default()
        };
        let mut app = tui(&chosen);
        for (width, drawn) in [(120, 50), (80, 40), (120, 50)] {
            screen(&mut app, width, 24);
            let edge = Rect::new(drawn - 1, 0, 1, 24);
            assert_eq!(app.sidebar.border, edge, "{width}");
            assert_eq!(app.composer_area.x, drawn + 1, "{width}");
            assert_eq!(app.sidebar_width, Some(50));
        }
        assert_eq!(crate::settings::saved_in_test(), Settings::default());
    }

    #[test]
    fn dragging_the_edge_resizes_the_sidebar_and_saves_once_on_release() {
        let mut app = tui(&Settings::default());
        let threads = vec![listed("a", "Alpha", "idle"), listed("b", "Bravo", "idle")];
        app.on_shell_event(shell_snapshot(threads));
        assert!(app.sidebar.select("b"));
        app.open = Some(long_thread());
        // A draft with the cursor between its two letters.
        app.focus = Focus::Composer;
        press(&mut app, KeyCode::Char('h'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Left);
        screen(&mut app, 120, 30);
        // A quarter of 120 columns, with the edge in the last of them.
        assert_eq!(app.sidebar.border, Rect::new(29, 0, 1, 30));

        // The press starts the drag and changes nothing on screen.
        assert!(!mouse(&mut app, CLICK, 29, 12));
        assert_eq!(app.drag.map(|d| (d.from, d.width)), Some((30, 30)));
        assert!(!app.layout_changed);
        // The edge follows the pointer between 20 columns and half the screen. Each move that
        // changes the width draws at once, so the edge is where the next report expects it.
        let drag = MouseEventKind::Drag(MouseButton::Left);
        for (x, width) in [(39, 40), (u16::MAX, 60), (2, 20)] {
            assert!(mouse(&mut app, drag, x, 3), "{x}");
            assert!(app.layout_changed);
            screen(&mut app, 120, 30);
            assert_eq!(app.sidebar.border.x + 1, width, "{x}");
            assert_eq!(app.composer_area.x, width + 1, "{x}");
        }
        // Past the narrowest width the edge stays put, and nothing needs drawing.
        assert!(!mouse(&mut app, drag, 0, 3));
        assert!(!app.layout_changed);
        assert!(mouse(&mut app, drag, 44, 3));
        screen(&mut app, 120, 30);

        // Nothing is saved until the release, which saves the width once.
        assert_eq!(app.sidebar_width, None);
        assert_eq!(crate::settings::saved_in_test(), Settings::default());
        let up = MouseEventKind::Up(MouseButton::Left);
        assert!(!mouse(&mut app, up, 44, 3));
        assert_eq!(app.drag, None);
        assert_eq!(app.sidebar_width, Some(45));
        assert_eq!(crate::settings::saved_in_test().sidebar_width, Some(45));

        // Focus, the highlight, the open thread, the draft and its cursor are as they were.
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.sidebar.selected_thread_id(), Some("b"));
        assert_eq!(app.open.as_ref().map(|open| open.id.as_str()), Some("t"));
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.composer.text(), "hid");
        // The cards take clicks at their new width.
        let rows = screen(&mut app, 120, 30);
        assert_eq!(app.sidebar.list.width, 45 - 3);
        let bravo = row_showing(&rows, "Bravo");
        assert_eq!(app.sidebar.thread_at(bravo), Some("b"));

        // A release over Alpha's card, with the edge held at the narrowest, ends the drag and
        // opens nothing.
        mouse(&mut app, CLICK, 44, 3);
        mouse(&mut app, drag, 0, 3);
        let rows = screen(&mut app, 120, 30);
        let alpha = row_showing(&rows, "Alpha");
        assert_eq!(app.sidebar.thread_at(alpha), Some("a"));
        assert!(!mouse(&mut app, up, 5, alpha));
        assert_eq!(app.drag, None);
        assert_eq!(app.sidebar.selected_thread_id(), Some("b"));
        assert_eq!(app.open.as_ref().map(|open| open.id.as_str()), Some("t"));
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(crate::settings::saved_in_test().sidebar_width, Some(20));
    }

    #[test]
    fn a_click_on_the_edge_opens_nothing_and_saves_nothing() {
        let mut app = tui(&Settings::default());
        let threads = vec![listed("a", "Alpha", "idle"), listed("b", "Bravo", "idle")];
        app.on_shell_event(shell_snapshot(threads));
        assert!(app.sidebar.select("a"));
        let rows = screen(&mut app, 80, 24);
        let bravo = row_showing(&rows, "Bravo");
        let edge = app.sidebar.border.x;
        assert_eq!(edge, 25);
        let drag = MouseEventKind::Drag(MouseButton::Left);
        let up = MouseEventKind::Up(MouseButton::Left);

        // Pressed and released on the edge beside Bravo's card, it opens nothing and moves
        // neither the highlight nor focus. It doesn't save the width t3term picked, either.
        assert!(!mouse(&mut app, CLICK, edge, bravo));
        assert!(!mouse(&mut app, up, edge, bravo));
        assert_eq!(app.drag, None);
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(app.open.is_none());
        assert_eq!(app.sidebar.selected_thread_id(), Some("a"));
        // Nor does a drag that comes back to where it started.
        mouse(&mut app, CLICK, edge, bravo);
        assert!(mouse(&mut app, drag, edge + 9, bravo));
        assert!(mouse(&mut app, drag, edge, bravo));
        assert!(!mouse(&mut app, up, edge, bravo));
        assert_eq!(app.sidebar_width, None);
        assert_eq!(crate::settings::saved_in_test(), Settings::default());

        // Motion or a release with no press on the edge first, and the other buttons, start no
        // drag, and a left drag that follows them moves nothing.
        let others = [
            drag,
            up,
            MouseEventKind::Moved,
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::Drag(MouseButton::Right),
        ];
        for kind in others {
            mouse(&mut app, kind, edge, bravo);
            assert_eq!(app.drag, None, "{kind:?}");
            assert!(!mouse(&mut app, drag, 50, bravo), "{kind:?}");
        }
        screen(&mut app, 80, 24);
        assert_eq!(app.sidebar.border.x, edge);
        // The column left of the edge is the cards' padding, not the edge.
        mouse(&mut app, CLICK, edge - 1, bravo);
        assert_eq!(app.drag, None);

        // During a drag the wheel and the other buttons do nothing, so the transcript under the
        // pointer doesn't scroll, and only the left button's release ends it.
        mouse(&mut app, CLICK, edge, bravo);
        mouse(&mut app, drag, edge + 5, bravo);
        let waiting = [
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Drag(MouseButton::Right),
            MouseEventKind::Up(MouseButton::Right),
            MouseEventKind::Up(MouseButton::Middle),
        ];
        let transcript = app.transcript_area;
        for kind in waiting {
            assert!(!mouse(&mut app, kind, transcript.x + 2, transcript.y + 2));
        }
        assert_eq!(app.drag.map(|d| (d.from, d.width)), Some((26, 31)));
        assert_eq!(app.scroll.rows(), 0);
        assert!(app.open.is_none());
    }

    #[test]
    fn a_drag_ends_without_its_width_on_a_key_ctrl_b_or_a_resize() {
        let mut app = tui(&Settings::default());
        app.focus = Focus::Composer;
        screen(&mut app, 80, 24);
        assert_eq!(app.sidebar.border.x, 25);

        // Esc puts the width back and does nothing else, so focus stays in the composer.
        drag_edge(&mut app, 35);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.drag, None);
        assert!(app.layout_changed, "the old width draws at once");
        assert_eq!(app.focus, Focus::Composer);
        screen(&mut app, 80, 24);
        assert_eq!(app.sidebar.border.x, 25);

        // Another key puts it back and then does what it always does.
        drag_edge(&mut app, 35);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.drag, None);
        assert_eq!(app.composer.text(), "x");
        // A release after that belongs to no drag.
        let up = MouseEventKind::Up(MouseButton::Left);
        assert!(!mouse(&mut app, up, 35, 5));
        screen(&mut app, 80, 24);
        assert_eq!(app.sidebar.border.x, 25);

        // Ctrl+B hides the sidebar, which comes back at its width from before the drag.
        drag_edge(&mut app, 35);
        ctrl_b(&mut app);
        assert!(app.sidebar_hidden);
        assert_eq!(app.drag, None);
        screen(&mut app, 80, 24);
        assert_eq!(app.sidebar.border, Rect::default());
        ctrl_b(&mut app);
        screen(&mut app, 80, 24);
        assert_eq!(app.sidebar.border.x, 25);

        // A resize ends it too. Until the next frame draws the new edge, the old one takes no
        // press.
        drag_edge(&mut app, 35);
        assert!(app.on_terminal_event(Event::Resize(120, 30)));
        assert_eq!(app.drag, None);
        assert_eq!(app.sidebar.border, Rect::default());
        mouse(&mut app, CLICK, 35, 5);
        assert_eq!(app.drag, None);
        screen(&mut app, 120, 30);
        assert_eq!(app.sidebar.border.x, 29);

        // None of them saved a width.
        assert_eq!(app.sidebar_width, None);
        assert_eq!(crate::settings::saved_in_test().sidebar_width, None);
    }

    #[test]
    fn a_release_the_terminal_never_reported_ends_the_drag_as_one_would() {
        let mut app = tui(&Settings::default());
        app.open = Some(long_thread());
        app.focus = Focus::Composer;
        screen(&mut app, 80, 24);

        // Motion with no button held means the button came up unreported, outside the window
        // say. The drag ends with the width it reached.
        drag_edge(&mut app, 35);
        assert!(!mouse(&mut app, MouseEventKind::Moved, 70, 5));
        assert_eq!(app.drag, None);
        assert_eq!(app.sidebar_width, Some(36));
        assert_eq!(crate::settings::saved_in_test().sidebar_width, Some(36));

        // So does a new press, which then does what it does. In the transcript it focuses it.
        drag_edge(&mut app, 39);
        let transcript = app.transcript_area;
        assert!(mouse(&mut app, CLICK, transcript.x + 2, transcript.y + 2));
        assert_eq!(app.drag, None);
        assert_eq!(app.focus, Focus::Transcript);
        assert_eq!(app.sidebar_width, Some(40));
        assert_eq!(crate::settings::saved_in_test().sidebar_width, Some(40));
    }

    #[test]
    fn brackets_in_the_sidebar_resize_it_and_zero_puts_back_the_default() {
        let mut app = tui(&Settings::default());
        assert_eq!(app.focus, Focus::Sidebar);
        let rows = screen(&mut app, 120, 30);
        assert!(rows[29].contains("[ ] width"), "{:?}", rows[29]);
        assert_eq!(app.sidebar.border.x + 1, 30);

        // Each press moves the edge two columns, saves the width and draws at once.
        press(&mut app, KeyCode::Char(']'));
        assert!(app.layout_changed);
        assert_eq!(app.sidebar_width, Some(32));
        assert_eq!(crate::settings::saved_in_test().sidebar_width, Some(32));
        screen(&mut app, 120, 30);
        assert_eq!(app.sidebar.border.x + 1, 32);
        for _ in 0..3 {
            press(&mut app, KeyCode::Char('['));
        }
        assert_eq!(app.sidebar_width, Some(26));
        // It stops at 20 columns and at half the screen. A press there changes nothing.
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('['));
        }
        assert_eq!(app.sidebar_width, Some(20));
        screen(&mut app, 120, 30);
        press(&mut app, KeyCode::Char('['));
        assert!(!app.layout_changed);
        for _ in 0..30 {
            press(&mut app, KeyCode::Char(']'));
        }
        assert_eq!(app.sidebar_width, Some(60));
        assert_eq!(crate::settings::saved_in_test().sidebar_width, Some(60));

        // `0` forgets the width, and the sidebar takes a quarter of the screen again.
        press(&mut app, KeyCode::Char('0'));
        assert!(app.layout_changed);
        assert_eq!(app.sidebar_width, None);
        assert_eq!(crate::settings::saved_in_test().sidebar_width, None);
        screen(&mut app, 120, 30);
        assert_eq!(app.sidebar.border.x + 1, 30);
        press(&mut app, KeyCode::Char('0'));
        assert!(!app.layout_changed, "there was nothing to forget");

        // The composer types them, the transcript ignores them, and an open menu takes them.
        app.focus = Focus::Composer;
        for c in ['[', ']', '0'] {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.composer.text(), "[]0");
        app.focus = Focus::Transcript;
        for c in ['[', ']', '0'] {
            press(&mut app, KeyCode::Char(c));
        }
        app.focus = Focus::Sidebar;
        app.picker = Some(mode_menu());
        press(&mut app, KeyCode::Char(']'));
        assert!(app.picker.is_some());
        assert_eq!(app.sidebar_width, None);
        assert!(!app.layout_changed);

        // Saved wider than 80 columns allow, `]` has nowhere to go and keeps the 50 for a
        // wider screen. `[` starts from the 40 columns on screen.
        let chosen = Settings {
            sidebar_width: Some(50),
            ..Settings::default()
        };
        let mut app = tui(&chosen);
        screen(&mut app, 80, 24);
        press(&mut app, KeyCode::Char(']'));
        assert_eq!(app.sidebar_width, Some(50));
        press(&mut app, KeyCode::Char('['));
        assert_eq!(app.sidebar_width, Some(38));
    }

    #[test]
    fn resizing_the_sidebar_rewraps_the_transcript_and_keeps_the_reader_in_place() {
        let mut app = tui(&Settings::default());
        app.open = Some(long_thread());
        app.composer.insert_str("draft");
        app.focus = Focus::Transcript;
        screen(&mut app, 80, 24);
        for _ in 0..3 {
            press(&mut app, KeyCode::PageUp);
        }
        screen(&mut app, 80, 24);
        let (top, _) = app.anchor.clone().expect("scrolled up");
        let prepared = &app.open.as_ref().unwrap().prepared;
        let (made, versions) = (prepared.made, prepared.versions.clone());
        let up = MouseEventKind::Up(MouseButton::Left);

        // The sidebar wider and then narrower than at the start, so the transcript narrows and
        // then widens.
        for (to, narrower) in [(35, true), (21, false)] {
            let before = app.transcript_area.width;
            drag_edge(&mut app, to);
            assert!(!mouse(&mut app, up, to, 5));
            assert_eq!(app.focus, Focus::Transcript, "the transcript keeps focus");
            screen(&mut app, 80, 24);
            assert_eq!(app.transcript_area.width < before, narrower);
            // Every block is wrapped again at the new width, from the blocks already made.
            let width = app.transcript_area.width - 1;
            assert!(app.cache.values().all(|cached| cached.key.1 == width));
            let prepared = &app.open.as_ref().unwrap().prepared;
            assert_eq!(prepared.made, made, "a block was made again");
            assert_eq!(prepared.versions, versions);
            // The same answer is at the top, and the transcript is still scrolled up.
            assert_eq!(app.anchor.as_ref().map(|(id, _)| id), Some(&top));
            assert!(app.scroll.rows() > 0);
            assert_eq!(app.composer.text(), "draft");
        }
    }

    #[test]
    fn the_conversation_follows_the_edge_and_an_open_menu_takes_the_first_press() {
        let mut app = tui(&Settings::default());
        app.open = Some(long_thread());
        app.focus = Focus::Composer;
        app.picker = Some(mode_menu());
        screen(&mut app, 120, 30);
        let (chips, menu) = (app.chips.clone(), app.picker_area);
        assert!(!chips.is_empty());
        let edge = app.sidebar.border.x;

        // With the menu open, a press on the edge only closes it, as a press anywhere outside
        // the menu does.
        assert!(mouse(&mut app, CLICK, edge, 5));
        assert!(app.picker.is_none());
        assert_eq!(app.drag, None);

        // The next press drags the edge 20 columns right.
        drag_edge(&mut app, edge + 20);
        let up = MouseEventKind::Up(MouseButton::Left);
        mouse(&mut app, up, edge + 20, 5);
        app.picker = Some(mode_menu());
        screen(&mut app, 120, 30);
        // The conversation starts a column of padding after the edge, and all of it moved.
        let x = app.sidebar.border.right() + 1;
        assert_eq!(x, edge + 22);
        let parts = [
            app.sidebar_toggle,
            app.transcript_area,
            app.panel_area,
            app.composer_area,
        ];
        for area in parts {
            assert_eq!(area.x, x, "{area:?}");
        }
        assert_eq!(app.tasks.area.x, x + 1);
        let composer = app.composer_area;
        assert_eq!(app.chips.len(), chips.len());
        for ((area, kind), (before, _)) in app.chips.iter().zip(&chips) {
            assert!(area.x > before.x, "{kind:?} stayed at {before:?}");
            assert!(area.x > composer.x && area.right() < composer.right());
        }
        assert!(app.picker_area.x > menu.x);
        assert!(app.picker_area.right() <= composer.right());
    }

    #[test]
    fn a_hidden_sidebar_has_no_edge_and_keeps_its_width_for_later() {
        let saved = Settings {
            sidebar_hidden: true,
            sidebar_width: Some(40),
            ..Settings::default()
        };
        let mut app = tui(&saved);
        screen(&mut app, 120, 30);
        assert_eq!(app.sidebar.border, Rect::default());
        assert_eq!(app.composer_area.x, 1);
        // A press where the edge would be is the conversation's.
        mouse(&mut app, CLICK, 39, 5);
        assert_eq!(app.drag, None);
        // Called while hidden, what `[`, `]` and `0` do changes nothing.
        app.step_sidebar(true);
        app.step_sidebar(false);
        app.reset_sidebar_width();
        assert!(!app.layout_changed);
        assert_eq!(app.sidebar_width, Some(40));

        // Shown again, it has the width it was given.
        ctrl_b(&mut app);
        screen(&mut app, 120, 30);
        assert_eq!(app.sidebar.border, Rect::new(39, 0, 1, 30));
        assert_eq!(app.composer_area.x, 41);
    }

    // ---- file changes ----

    #[test]
    fn an_edit_updates_in_place_and_keeps_the_reader_where_they_were() {
        /// How many rows show `needle`.
        fn count(rows: &[String], needle: &str) -> usize {
            rows.iter().filter(|row| row.contains(needle)).count()
        }
        let event = |sequence: u64, payload: &Value| {
            WatchEvent::Item(json!({"kind": "event", "sequence": sequence, "event": {
                "type": "turn-item.updated",
                "payload": payload,
            }}))
        };
        let mut app = tui(&Settings::default());
        app.open = Some(long_thread());
        app.composer.insert_str("draft");
        app.focus = Focus::Transcript;
        // After the twenty answers, a run checks the code and starts an ACP edit of two files.
        let command = json!({"id": "cmd-1", "runId": "r1", "type": "command_execution",
            "ordinal": 20, "status": "completed", "title": null, "input": "cargo check",
            "exitCode": 0, "updatedAt": "2026-10-09T10:00:00.000Z"});
        let running = json!({
            "id": "edit-1",
            "runId": "r1",
            "type": "file_change",
            "ordinal": 21,
            "status": "running",
            "title": null,
            "fileName": "/workspace/new.ts",
            "additions": 4,
            "deletions": 2,
            "changes": [
                {"operation": "move", "path": "/workspace/new.ts",
                    "oldPath": "/workspace/old.ts"},
                {"operation": "add", "path": "/workspace/notes.md", "fileType": "text"},
            ],
            "updatedAt": "2026-10-09T10:00:01.000Z",
        });
        app.on_thread_event(event(2, &command));
        app.on_thread_event(event(3, &running));

        // Closed, the group is one row, and the edit's lines wait under it.
        let rows = screen(&mut app, 120, 30);
        row_showing(&rows, "› 2 tool calls · Run, Edit");
        assert_eq!(count(&rows, "/workspace/old.ts"), 0);

        // Opened, the edit's row counts its files, and each operation has a line under it.
        app.open_bundles.insert("cmd-1".into());
        let rows = screen(&mut app, 120, 30);
        let edit = row_showing(&rows, "✎ Edit   2 files  +4 -2") as usize;
        assert!(rows[edit + 1].contains("│ move /workspace/old.ts → /workspace/new.ts"));
        assert!(rows[edit + 2].contains("│ add /workspace/notes.md (text)"));
        let made = app.open.as_ref().unwrap().prepared.made;

        // Scrolled up, the reader stays on the answer they were reading when the edit below
        // fails and gains a line. Only the edit's block is made again.
        for _ in 0..3 {
            press(&mut app, KeyCode::PageUp);
        }
        screen(&mut app, 120, 30);
        let (top, _) = app.anchor.clone().expect("scrolled up");
        let mut failed = running;
        failed["status"] = json!("failed");
        failed["diffStr"] = json!("String to replace not found");
        failed["updatedAt"] = json!("2026-10-09T10:00:02.000Z");
        app.on_thread_event(event(4, &failed));
        screen(&mut app, 120, 30);
        assert_eq!(app.anchor.as_ref().map(|(id, _)| id), Some(&top));
        assert!(app.scroll.rows() > 0);
        assert_eq!(app.open.as_ref().unwrap().prepared.made, made + 1);

        // At the bottom, the error comes first under the row, and nothing is drawn twice.
        press(&mut app, KeyCode::End);
        let rows = screen(&mut app, 120, 30);
        let edit = row_showing(&rows, "✎ Edit   2 files  +4 -2") as usize;
        assert!(rows[edit + 1].contains("│ String to replace not found"));
        assert!(rows[edit + 2].contains("│ move /workspace/old.ts → /workspace/new.ts"));
        for needle in ["✎ Edit", "to replace", "old.ts", "notes.md"] {
            assert_eq!(count(&rows, needle), 1, "{needle}: {rows:#?}");
        }

        // A reconnect that replays the event changes nothing.
        app.on_thread_event(event(4, &failed));
        let rows = screen(&mut app, 120, 30);
        assert_eq!(count(&rows, "String to replace not found"), 1);
        assert_eq!(app.open.as_ref().unwrap().prepared.made, made + 1);

        // Hiding the sidebar and showing it again rewraps every row from the blocks already
        // made, and the draft and focus stay.
        for hidden in [true, false] {
            ctrl_b(&mut app);
            let rows = screen(&mut app, 120, 30);
            assert_eq!(app.sidebar_hidden, hidden);
            let width = app.transcript_area.width - 1;
            assert!(app.cache.values().all(|cached| cached.key.1 == width));
            assert_eq!(app.open.as_ref().unwrap().prepared.made, made + 1);
            assert_eq!(count(&rows, "String to replace not found"), 1);
            assert_eq!(app.composer.text(), "draft");
            assert_eq!(app.focus, Focus::Transcript);
        }

        // Verbose mode opens the group as well.
        app.open_bundles.clear();
        app.verbose = true;
        let rows = screen(&mut app, 120, 30);
        assert_eq!(count(&rows, "String to replace not found"), 1);
    }

    // ---- the thread error banner ----

    /// Twenty answers long enough to wrap to a different height at each width.
    fn answers() -> Vec<Value> {
        (0..20)
            .map(|i| {
                json!({
                    "id": format!("a{i}"),
                    "type": "assistant_message",
                    "ordinal": i,
                    "text": format!("Answer {i}: {}", "word ".repeat(60)),
                })
            })
            .collect()
    }

    /// The Codex provider session, with `last_error`.
    fn codex_session(last_error: Value) -> Value {
        json!({"id": "s1", "providerInstanceId": "codex", "lastError": last_error})
    }

    /// A snapshot of thread `id` on Codex with a provider thread, so the Codex session's
    /// `lastError`, when it has one, is the thread's error. Twenty answers fill the transcript,
    /// and `more` adds to the projection or replaces its lists.
    fn codex_snapshot(id: &str, last_error: Option<&str>, more: Value) -> Value {
        let sessions: Vec<Value> = last_error
            .map(|error| codex_session(json!(error)))
            .into_iter()
            .collect();
        let mut projection = json!({
            "thread": {"id": id, "providerInstanceId": "codex", "activeProviderThreadId": "pt"},
            "turnItems": answers(),
            "providerSessions": sessions,
        });
        if let (Some(projection), Value::Object(more)) = (projection.as_object_mut(), more) {
            projection.extend(more);
        }
        json!({"kind": "snapshot", "snapshotSequence": 1, "projection": projection})
    }

    /// Thread `id`, open and live as `codex_snapshot` has it.
    fn codex_thread(id: &str, last_error: Option<&str>, more: Value) -> OpenThread {
        let snapshot = codex_snapshot(id, last_error, more);
        let mut open = OpenThread::new(id.into(), mpsc::unbounded_channel().1);
        assert_eq!(open.apply(&snapshot), Applied::Snapshot);
        open.connection = "live".into();
        open
    }

    /// A live event on the open thread, as its watch delivers it.
    fn live(sequence: u64, kind: &str, payload: Value) -> WatchEvent {
        let event = json!({"type": kind, "payload": payload});
        let item = json!({"kind": "event", "sequence": sequence, "event": event});
        WatchEvent::Item(item)
    }

    /// The Codex session's `lastError` changing to `error`.
    fn session_error(sequence: u64, error: Value) -> WatchEvent {
        live(sequence, "provider-session.updated", codex_session(error))
    }

    /// A send to `thread` that failed, as the send's task reports it.
    fn send_failed(thread: &str, message_id: &str, text: &str) -> ActionResult {
        ActionResult::SendFailed {
            thread_id: thread.into(),
            message_id: message_id.into(),
            text: text.into(),
            error: "T3 refused the message".into(),
            reserved: Vec::new(),
        }
    }

    /// Alt and a letter, as terminals also report Esc and a quick letter.
    fn alt(app: &mut App, c: char) {
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
        app.on_terminal_event(Event::Key(key));
    }

    /// Draws one frame as `screen` does and returns its cells.
    fn cells(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let frame = terminal.draw(|frame| app.draw(frame)).unwrap();
        frame.buffer.clone()
    }

    /// The text `buffer` holds in `area`, a line for each row.
    fn text_in(buffer: &ratatui::buffer::Buffer, area: Rect) -> String {
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Draws a frame and says whether it has a banner.
    fn banner_shows(app: &mut App) -> bool {
        screen(app, 120, 30);
        app.banner.showing()
    }

    #[test]
    fn an_error_comes_over_the_transcript_and_moves_none_of_it() {
        let mut app = tui(&Settings::default());
        app.open_thread(codex_thread("t", None, json!({})));
        app.focus = Focus::Transcript;
        app.scroll.set(12);
        let before = cells(&mut app, 120, 30);
        assert_eq!(app.banner.area, Rect::default(), "no error, no banner");
        let (anchor, body) = (app.anchor.clone(), app.transcript_area);

        app.on_thread_event(session_error(2, json!("Codex quit")));
        let after = cells(&mut app, 120, 30);
        let banner = app.banner.area;
        assert_eq!(banner.intersection(body), banner);
        assert_eq!(banner.y, body.y, "it covers the transcript's top rows");
        let text = text_in(&after, banner);
        assert!(text.contains("! Codex quit"), "{text}");
        assert!(text.contains('×'), "{text}");
        assert!(text.contains("Alt+W dismiss"), "{text}");
        // The rest of the transcript is as it was, and the reader is where they were.
        for y in body.y..body.bottom() {
            for x in body.x..body.right() {
                if !banner.contains(ratatui::layout::Position::new(x, y)) {
                    assert_eq!(after[(x, y)], before[(x, y)], "({x}, {y})");
                }
            }
        }
        assert_eq!(app.anchor, anchor);
        assert_eq!(app.scroll.rows(), 12);
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn alt_w_or_a_click_on_the_close_button_dismisses_the_error_and_keeps_the_draft() {
        let mut app = tui(&Settings::default());
        app.open_thread(codex_thread("t", Some("Codex quit"), json!({})));
        app.composer.insert_str("draft");
        assert!(banner_shows(&mut app));

        alt(&mut app, 'w');
        let rows = screen(&mut app, 120, 30);
        assert_eq!(app.banner.area, Rect::default());
        assert!(rows.iter().all(|row| !row.contains("Alt+W dismiss")));
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.composer.text(), "draft", "the key typed nothing");
        // Nothing went to T3. The thread's session still has the error.
        let state = app.open.as_ref().unwrap().state.as_ref().unwrap();
        assert_eq!(state.list("providerSessions")[0]["lastError"], "Codex quit");
        // With no banner, Alt+W types nothing either.
        alt(&mut app, 'w');
        assert_eq!(app.composer.text(), "draft");

        // A new error shows. A w without Alt is the composer's.
        app.on_thread_event(session_error(2, json!("Codex quit again")));
        press(&mut app, KeyCode::Char('w'));
        assert!(banner_shows(&mut app));
        assert_eq!(app.composer.text(), "draftw");

        // A click on × dismisses it. Focus stays in the composer, where a click on the
        // transcript would have moved it.
        let close = app.banner.close;
        assert!(mouse(&mut app, CLICK, close.x + 1, close.y));
        assert!(!banner_shows(&mut app));
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.composer.text(), "draftw");
    }

    #[test]
    fn a_click_on_a_long_error_opens_it_and_alt_i_closes_it() {
        let mut app = tui(&Settings::default());
        let long = "the provider sent more than fits ".repeat(30);
        let long = format!("Codex quit: {long}");
        app.open_thread(codex_thread("t", Some(&long), json!({})));
        let buffer = cells(&mut app, 120, 30);
        let closed = app.banner.area;
        assert_eq!(closed.height, 5, "three rows and the border");
        let text = text_in(&buffer, closed);
        assert!(text.contains('…'), "{text}");
        assert!(text.contains("Alt+I more · Alt+W dismiss"), "{text}");
        // The wheel over a closed error scrolls the transcript under it.
        let (x, y) = (closed.x + 2, closed.y + 2);
        assert!(mouse(&mut app, MouseEventKind::ScrollUp, x, y));
        assert_eq!(app.scroll.rows(), 3);

        // A click on its text opens it, and focus stays in the composer.
        assert!(mouse(&mut app, CLICK, x, y));
        let buffer = cells(&mut app, 120, 30);
        let open = app.banner.area;
        assert!(open.height > closed.height, "{open:?}");
        assert_eq!(open.intersection(app.transcript_area), open);
        assert!(text_in(&buffer, open).contains("Alt+I less"));
        assert_eq!(app.focus, Focus::Composer);
        // Alt+I closes it, and types nothing.
        alt(&mut app, 'i');
        screen(&mut app, 120, 30);
        assert_eq!(app.banner.area, closed);
        assert_eq!(app.composer.text(), "");
        assert_eq!(app.scroll.rows(), 3);
    }

    #[test]
    fn a_click_on_the_banner_never_reaches_the_row_under_it() {
        let call = |id: &str, ordinal: u64| {
            json!({"id": id, "runId": "r1", "type": "command_execution", "ordinal": ordinal,
                "status": "completed", "title": null, "input": "cargo check", "exitCode": 0})
        };
        let mut items = answers();
        items[10] = call("c1", 10);
        items[11] = call("c2", 11);
        let mut app = tui(&Settings::default());
        let open = codex_thread("t", Some("Codex quit"), json!({"turnItems": items}));
        app.open_thread(open);
        // Scroll until the calls' row is under the banner.
        let mut under = None;
        for rows in 0..400 {
            app.scroll.set(rows);
            screen(&mut app, 120, 30);
            let banner = app.banner.area;
            under = app
                .bundle_rows
                .iter()
                .find(|(y, _)| (banner.y..banner.bottom()).contains(y))
                .map(|(y, _)| *y);
            if under.is_some() {
                break;
            }
        }
        let y = under.expect("a scroll puts the calls' row under the banner");
        let (banner, scrolled) = (app.banner.area, app.scroll.rows());

        // A short error has nothing to open, so the click does nothing at all.
        assert!(!mouse(&mut app, CLICK, banner.x + 1, y));
        assert!(app.open_bundles.is_empty());
        assert_eq!(app.focus, Focus::Composer);
        screen(&mut app, 120, 30);
        assert_eq!((app.banner.area, app.scroll.rows()), (banner, scrolled));
        // Beside the banner, the same row opens the calls.
        let x = app.transcript_area.x;
        assert!(mouse(&mut app, CLICK, x, y));
        assert!(!app.open_bundles.is_empty());
    }

    #[test]
    fn a_dismissed_error_stays_hidden_on_its_thread_and_shows_on_another() {
        let mut app = tui(&Settings::default());
        app.open_thread(codex_thread("a", Some("Codex quit"), json!({})));
        assert!(banner_shows(&mut app));
        alt(&mut app, 'w');
        assert!(!banner_shows(&mut app));
        // Another thread with the same error shows it.
        app.open_thread(codex_thread("b", Some("Codex quit"), json!({})));
        assert!(banner_shows(&mut app));
        // Back on the first, the error stays dismissed, as it does through a reconnect's
        // snapshot and a replayed event.
        app.open_thread(codex_thread("a", Some("Codex quit"), json!({})));
        assert!(!banner_shows(&mut app));
        let snapshot = codex_snapshot("a", Some("Codex quit"), json!({}));
        app.on_thread_event(WatchEvent::Item(snapshot));
        app.on_thread_event(session_error(2, json!("Codex quit")));
        app.on_thread_event(session_error(2, json!("Codex quit")));
        assert!(!banner_shows(&mut app));

        // A different error shows.
        app.on_thread_event(session_error(3, json!("Codex lost its connection")));
        assert!(banner_shows(&mut app));
        // An error that clears, or isn't text, hides it.
        let errors = [
            (4, Value::Null),
            (5, json!(7)),
            (6, json!("")),
            (7, json!(" ")),
        ];
        for (sequence, error) in errors {
            app.on_thread_event(session_error(sequence, error));
            assert!(!banner_shows(&mut app), "{sequence}");
        }
        // A detached session's error no longer stands for the thread.
        app.on_thread_event(session_error(8, json!("Codex lost its connection")));
        assert!(banner_shows(&mut app));
        let detach = json!({"providerSessionId": "s1"});
        app.on_thread_event(live(9, "provider-session.detached", detach));
        assert!(!banner_shows(&mut app));
    }

    #[test]
    fn a_failed_run_shows_its_error_until_a_new_run_starts() {
        let run = |id: &str, ordinal: u64, status: &str| {
            let root = format!("{id}-n");
            json!({"id": id, "ordinal": ordinal, "status": status, "rootNodeId": root})
        };
        let error = |class: &str| {
            let failure = json!({"message": "You hit the usage limit", "class": class});
            json!({"id": "e1", "type": "error", "status": "failed", "ordinal": 20, "runId": "r1",
                "nodeId": "r1-n", "failure": failure})
        };
        let mut app = tui(&Settings::default());
        app.open_thread(codex_thread("t", None, json!({})));
        app.on_thread_event(live(2, "run.created", run("r1", 1, "running")));
        app.on_thread_event(live(3, "turn-item.updated", error("provider_error")));
        assert!(!banner_shows(&mut app), "the run hasn't failed yet");

        app.on_thread_event(live(4, "run.updated", run("r1", 1, "failed")));
        let buffer = cells(&mut app, 120, 30);
        let banner = app.banner.area;
        assert!(text_in(&buffer, banner).contains("You hit the usage limit"));
        assert_eq!(buffer[(banner.x, banner.y)].fg, app.theme.error);

        // A usage limit is a warning, and stays the thread's outcome while a new run waits.
        app.on_thread_event(live(5, "turn-item.updated", error("usage_limit")));
        app.on_thread_event(live(6, "run.created", run("r2", 2, "queued")));
        let buffer = cells(&mut app, 120, 30);
        let banner = app.banner.area;
        assert!(text_in(&buffer, banner).contains("You hit the usage limit"));
        assert_eq!(buffer[(banner.x, banner.y)].fg, app.theme.warning);
        // Once the new run starts, the error is behind it.
        app.on_thread_event(live(7, "run.updated", run("r2", 2, "running")));
        assert!(!banner_shows(&mut app));
    }

    #[test]
    fn a_failed_send_shows_on_its_thread_and_the_message_comes_back() {
        let mut app = tui(&Settings::default());
        app.open_thread(codex_thread("a", None, json!({})));
        app.on_action_result(send_failed("a", "m1", "hello"));
        let buffer = cells(&mut app, 120, 30);
        let text = text_in(&buffer, app.banner.area);
        assert!(text.contains("T3 refused the message"), "{text}");
        // The message comes back to the composer, and the toast says why, as before.
        assert_eq!(app.composer.text(), "hello");
        let toast = app.message.clone().map(|(text, _)| text);
        assert_eq!(toast.as_deref(), Some("T3 refused the message"));
        // The toast goes with the next key. The banner stays until it is dismissed.
        press(&mut app, KeyCode::End);
        assert!(banner_shows(&mut app));
        assert!(app.message.is_none());
        alt(&mut app, 'w');
        assert!(!banner_shows(&mut app));
        assert_eq!(app.composer.text(), "hello", "kept for a retry");
    }

    #[test]
    fn a_send_that_fails_after_the_reader_moves_on_waits_for_its_thread() {
        let mut app = tui(&Settings::default());
        app.open_thread(codex_thread("b", None, json!({})));
        app.composer.insert_str("draft for b");
        app.on_action_result(send_failed("a", "m1", "hello a"));
        assert!(!banner_shows(&mut app), "the error is a's");
        assert_eq!(app.composer.text(), "draft for b");
        assert!(app.unsent.has_saved("a"));
        let toast = app.message.clone().unwrap_or_default().0;
        assert!(toast.contains("Ctrl+R in its thread"), "{toast}");

        // On its thread the banner shows it, and Ctrl+R swaps the message with the draft.
        app.open_thread(codex_thread("a", None, json!({})));
        let buffer = cells(&mut app, 120, 30);
        let text = text_in(&buffer, app.banner.area);
        assert!(text.contains("T3 refused the message"), "{text}");
        assert_eq!(app.composer.text(), "draft for b");
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        app.on_terminal_event(Event::Key(ctrl_r));
        assert_eq!(app.composer.text(), "hello a");
        assert!(app.unsent.has_saved("a"), "the draft waits in its place");
        // Away and back, it is still there.
        app.open_thread(codex_thread("b", None, json!({})));
        assert!(!banner_shows(&mut app));
        app.open_thread(codex_thread("a", None, json!({})));
        assert!(banner_shows(&mut app));

        // The message reached T3 after all, so its error goes.
        let message = json!({"id": "m1", "role": "user", "text": "hello a"});
        app.on_thread_event(live(2, "message.updated", message));
        assert!(!banner_shows(&mut app));
    }

    #[test]
    fn a_send_error_stands_before_the_threads_own_until_it_is_dismissed() {
        let mut app = tui(&Settings::default());
        app.open_thread(codex_thread("t", Some("Codex quit"), json!({})));
        app.on_action_result(send_failed("t", "m1", "hello"));
        let buffer = cells(&mut app, 120, 30);
        let text = text_in(&buffer, app.banner.area);
        assert!(text.contains("T3 refused the message"), "{text}");
        assert!(!text.contains("Codex quit"), "{text}");
        // Dismissing the send's error shows the thread's own.
        alt(&mut app, 'w');
        let buffer = cells(&mut app, 120, 30);
        assert!(text_in(&buffer, app.banner.area).contains("Codex quit"));
        alt(&mut app, 'w');
        assert!(!banner_shows(&mut app));
    }

    #[test]
    fn an_open_menu_takes_the_first_click_and_the_banners_keys_still_work() {
        let mut app = tui(&Settings::default());
        app.open_thread(codex_thread("t", Some("Codex quit"), json!({})));
        app.picker = Some(mode_menu());
        screen(&mut app, 120, 30);
        let close = app.banner.close;
        let on_close = ratatui::layout::Position::new(close.x + 1, close.y);
        assert!(!app.picker_area.contains(on_close), "the menu covers ×");
        assert!(mouse(&mut app, CLICK, on_close.x, on_close.y));
        assert!(app.picker.is_none());
        assert!(banner_shows(&mut app), "the click only closed the menu");

        app.picker = Some(mode_menu());
        alt(&mut app, 'w');
        assert!(app.picker.is_some(), "the menu stays open");
        assert!(!banner_shows(&mut app));
    }

    #[test]
    fn the_banner_keeps_to_the_transcript_beside_a_request_the_tasks_and_the_sidebar() {
        let request = json!({"id": "q1", "kind": "command", "status": "pending"});
        let request = json!({"runtimeRequests": [request]});
        let steps = json!([
            {"id": "s1", "text": "Read the code", "status": "completed"},
            {"id": "s2", "text": "Fix the bug", "status": "running"},
        ]);
        let list = json!({"id": "l1", "threadId": "t", "runId": null, "nodeId": "n",
            "kind": "todo_list", "status": "active", "steps": steps});
        let tasks = json!({"plans": [list]});
        for (more, panel) in [(request, true), (tasks, false)] {
            let mut app = tui(&Settings::default());
            app.open_thread(codex_thread("t", Some("Codex quit"), more));
            for (width, height) in [(120, 30), (60, 16), (40, 12)] {
                screen(&mut app, width, height);
                let banner = app.banner.area;
                let other = if panel {
                    app.panel_area
                } else {
                    app.tasks.area
                };
                if width == 120 {
                    assert!(banner.height > 0 && other.height > 0, "{other:?}");
                }
                if banner.is_empty() {
                    continue;
                }
                assert_eq!(banner.intersection(app.transcript_area), banner);
                let drawn = [
                    other,
                    app.composer_area,
                    app.sidebar.list,
                    app.sidebar_toggle,
                ];
                for area in drawn {
                    assert!(!banner.intersects(area), "{width}x{height}: {area:?}");
                }
            }
            screen(&mut app, 120, 30);
            if panel {
                // A closed banner leaves Alt+↓ to the request panel.
                let down = KeyEvent::new(KeyCode::Down, KeyModifiers::ALT);
                app.on_terminal_event(Event::Key(down));
                assert_eq!(app.panel_scroll, 1);
            } else {
                // Alt+T is still the drawer's.
                alt(&mut app, 't');
                assert!(app.tasks.open);
            }
            assert!(banner_shows(&mut app));
            assert_eq!(app.composer.text(), "");
        }
    }

    #[test]
    fn any_screen_size_draws_the_banner_inside_the_transcript() {
        let error = format!(
            "{}\n\u{1b}[31m{}\u{1b}[0m\t👩‍💻 e\u{301}\r\n{}",
            "界".repeat(200),
            "a line that goes on ".repeat(30),
            "x".repeat(5000)
        );
        let small = [(0, 0), (1, 1), (10, 5), (19, 3), (26, 4), (40, 8)];
        // Growing, then shrinking, so nothing from a larger frame is left behind.
        let large = [(80, 24), (200, 60), (26, 4), (80, 24)];
        let sizes = small.into_iter().chain(large);
        for hidden in [false, true] {
            let settings = Settings {
                sidebar_hidden: hidden,
                ..Settings::default()
            };
            let mut app = tui(&settings);
            app.open_thread(codex_thread("t", Some(&error), json!({})));
            app.composer.insert_str("a draft that wraps when narrow");
            let mut opened = false;
            for (width, height) in sizes.clone() {
                for _ in 0..2 {
                    let buffer = cells(&mut app, width, height);
                    let (banner, close) = (app.banner.area, app.banner.close);
                    if banner.is_empty() {
                        assert_eq!(close, Rect::default(), "{width}x{height}");
                        continue;
                    }
                    let body = app.transcript_area;
                    assert_eq!(banner.intersection(body), banner, "{width}x{height}");
                    assert_eq!(close.intersection(banner), close, "{width}x{height}");
                    assert!(text_in(&buffer, close).contains('×'), "{width}x{height}");
                    // Closed, it takes at most half the transcript. Open, it may take all of it.
                    let half = (body.height / 2).max(1);
                    assert!(opened || banner.height <= half, "{width}x{height}");
                    let printable = |c: char| c == '\n' || !c.is_control();
                    let text = text_in(&buffer, banner);
                    assert!(text.chars().all(printable), "{text:?}");
                    if !opened && width >= 80 {
                        alt(&mut app, 'i');
                        opened = true;
                    }
                }
            }
            assert!(opened);
            assert_eq!(app.composer.text(), "a draft that wraps when narrow");
        }
    }

    /// How many times the thread's error was worked out, the banner followed the thread, and
    /// the banner took up an error it didn't have, which looks it up among those dismissed and
    /// cleans its text.
    fn banner_work(app: &App) -> (usize, usize, usize) {
        let derives = app.open.as_ref().map_or(0, |open| open.error.derives);
        (derives, app.banner.follows, app.banner.reads)
    }

    /// `pieces` updates of one answer in run r1, from `sequence` on, with a frame after each.
    fn stream(app: &mut App, sequence: u64, pieces: u64) {
        for piece in 0..pieces {
            let text = format!("Answer: {}", "word ".repeat(piece as usize));
            let answer = json!({"id": "streamed", "type": "assistant_message", "runId": "r1",
                "ordinal": 30, "text": text});
            app.on_thread_event(live(sequence + piece, "turn-item.updated", answer));
            screen(app, 120, 30);
        }
    }

    #[test]
    fn a_streamed_answer_never_reads_the_threads_error_again() {
        // A provider session's error has no limit. This one is over a megabyte, of which the
        // banner prints 4,096 bytes.
        let long = format!("Codex quit: {}", "the provider said more ".repeat(50_000));
        let mut app = tui(&Settings::default());
        app.open_thread(codex_thread("t", Some(&long), json!({})));
        // The thread just opened, so its error is read once.
        assert!(banner_shows(&mut app));
        assert_eq!(banner_work(&app), (1, 1, 1));
        // A run event works the thread's error out again, and the banner only compares it.
        let run = json!({"id": "r1", "ordinal": 1, "status": "running", "rootNodeId": "r1-n"});
        app.on_thread_event(live(2, "run.created", run.clone()));
        assert_eq!(banner_work(&app), (2, 2, 1));

        // An answer streams in a hundred pieces with a frame after each, a message lands, the
        // watch reconnects, and the reader opens and scrolls the error. None of it reads it.
        stream(&mut app, 3, 100);
        let message = json!({"id": "m9", "role": "user", "text": "hello"});
        app.on_thread_event(live(103, "message.updated", message));
        app.on_thread_event(WatchEvent::Reconnecting {
            reason: "socket closed".into(),
            retry_in: Duration::from_secs(1),
        });
        alt(&mut app, 'i');
        screen(&mut app, 120, 30);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::ALT);
        app.on_terminal_event(Event::Key(down));
        let rows = screen(&mut app, 120, 30);
        assert!(rows.iter().any(|row| row.contains("Lines 2-")), "{rows:#?}");
        assert_eq!(banner_work(&app), (2, 2, 1));

        // A blank error, or one of control characters only, shows nothing. The banner takes it
        // up once, and neither the answer going on nor another run event takes it up again.
        let blank = " \t\u{1b}\u{7}\r\n".repeat(100_000);
        app.on_thread_event(session_error(104, json!(blank)));
        assert!(!banner_shows(&mut app));
        assert_eq!(banner_work(&app), (3, 3, 2));
        stream(&mut app, 105, 100);
        assert_eq!(banner_work(&app), (3, 3, 2));
        app.on_thread_event(live(205, "run.updated", run));
        assert!(!banner_shows(&mut app));
        assert_eq!(banner_work(&app), (4, 4, 2));

        // The long error again, which the reader dismisses. It stays dismissed when T3 sends it
        // again, and an error that differs from it only past what the banner prints shows.
        app.on_thread_event(session_error(206, json!(long)));
        assert!(banner_shows(&mut app));
        assert_eq!(banner_work(&app), (5, 5, 3));
        alt(&mut app, 'w');
        assert!(!banner_shows(&mut app));
        // The dismissal follows the thread again, which only compares the error.
        assert_eq!(banner_work(&app), (5, 6, 3));
        stream(&mut app, 207, 20);
        app.on_thread_event(session_error(227, json!(long)));
        assert!(!banner_shows(&mut app));
        assert_eq!(banner_work(&app), (6, 7, 3));
        let tail = format!("{long} and one more line");
        app.on_thread_event(session_error(228, json!(tail)));
        assert!(banner_shows(&mut app));
        assert_eq!(banner_work(&app), (7, 8, 4));

        // A reconnect's snapshot reads the thread again and keeps the banner as it was.
        let snapshot = codex_snapshot("t", Some(&tail), json!({}));
        app.on_thread_event(WatchEvent::Item(snapshot));
        assert!(banner_shows(&mut app));
        assert_eq!(banner_work(&app), (8, 9, 4));
    }

    #[test]
    fn a_new_send_drops_the_last_ones_error() {
        // Enter spawns the send on this runtime, which never runs it, so nothing connects.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(codex_thread("t", Some("Codex quit"), json!({})));
        app.on_action_result(send_failed("t", "m1", "hello"));
        let buffer = cells(&mut app, 120, 30);
        let text = text_in(&buffer, app.banner.area);
        assert!(text.contains("T3 refused the message"), "{text}");
        assert_eq!(app.composer.text(), "hello");

        // Sending it again drops its error, and the thread's own shows until this send fails
        // too.
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.composer.text(), "");
        let buffer = cells(&mut app, 120, 30);
        let text = text_in(&buffer, app.banner.area);
        assert!(text.contains("Codex quit"), "{text}");
        app.on_action_result(send_failed("t", "m2", "hello"));
        let buffer = cells(&mut app, 120, 30);
        let text = text_in(&buffer, app.banner.area);
        assert!(text.contains("T3 refused the message"), "{text}");
        assert_eq!(app.composer.text(), "hello");

        // Once the send's error is dismissed, the next send has none to drop, so the banner
        // keeps the thread's own without reading it again.
        alt(&mut app, 'w');
        let buffer = cells(&mut app, 120, 30);
        assert!(text_in(&buffer, app.banner.area).contains("Codex quit"));
        let before = banner_work(&app);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.composer.text(), "");
        assert!(banner_shows(&mut app));
        assert_eq!(banner_work(&app), before);
    }

    // ---- live server configuration ----

    /// A runtime that is never driven. A read of T3's configuration that the TUI spawns on it
    /// waits without running, so nothing connects and `reads` can count it.
    fn idle_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    /// How many reads of T3's configuration the TUI has started on `runtime`. Nothing else in
    /// these tests spawns a task.
    fn reads(runtime: &tokio::runtime::Runtime) -> usize {
        runtime.metrics().num_alive_tasks()
    }

    /// A provider in T3's configuration with `models`, each a slug and a name.
    fn provider(id: &str, name: &str, status: &str, models: &[(&str, &str)]) -> Value {
        let models: Vec<Value> = models
            .iter()
            .map(|&(slug, title)| json!({"slug": slug, "name": title}))
            .collect();
        json!({"instanceId": id, "displayName": name, "enabled": true, "status": status,
            "models": models})
    }

    /// Codex, with GPT-6 and GPT-6 Mini.
    fn codex(status: &str) -> Value {
        let models = [("gpt-6", "GPT-6"), ("gpt-6-mini", "GPT-6 Mini")];
        provider("codex", "Codex", status, &models)
    }

    /// Claude, with Opus 5.5 and Haiku 4.5.
    fn claude(status: &str) -> Value {
        let models = [
            ("claude-opus-5-5", "Opus 5.5"),
            ("claude-haiku-4-5", "Haiku 4.5"),
        ];
        provider("claudeAgent", "Claude", status, &models)
    }

    /// A snapshot from the config subscription that lists `providers`, on a server with
    /// `capabilities`.
    fn config_snapshot(providers: Vec<Value>, capabilities: Value) -> WatchEvent {
        let environment = json!({"capabilities": capabilities});
        let config = json!({"providers": providers, "settings": {}, "environment": environment});
        let item = json!({"version": 1, "type": "snapshot", "config": config});
        WatchEvent::Item(item)
    }

    /// A change from the config subscription that lists every provider again.
    fn provider_statuses(providers: Vec<Value>) -> WatchEvent {
        let payload = json!({"providers": providers});
        let item = json!({"version": 1, "type": "providerStatuses", "payload": payload});
        WatchEvent::Item(item)
    }

    /// Thread `t`, open and live on Codex's GPT-6.
    fn thread_on_codex() -> OpenThread {
        let thread = json!({
            "id": "t",
            "modelSelection": {"instanceId": "codex", "model": "gpt-6"},
            "runtimeMode": "full-access",
            "interactionMode": "default",
        });
        let projection = json!({"thread": thread, "turnItems": []});
        let item = json!({"kind": "snapshot", "snapshotSequence": 1, "projection": projection});
        let mut open = OpenThread::new("t".into(), mpsc::unbounded_channel().1);
        assert_eq!(open.apply(&item), Applied::Snapshot);
        open.connection = "live".into();
        open
    }

    /// The model chip's text in a frame 120 columns by 30 rows.
    fn model_chip(app: &mut App) -> String {
        let buffer = cells(app, 120, 30);
        let chip = app.chips.iter().find(|(_, kind)| *kind == Kind::Model);
        text_in(&buffer, chip.expect("a model chip").0)
    }

    /// The open menu's text in a frame 120 columns by 30 rows.
    fn menu_text(app: &mut App) -> String {
        let buffer = cells(app, 120, 30);
        assert!(app.picker.is_some(), "no menu is open");
        text_in(&buffer, app.picker_area)
    }

    #[test]
    fn provider_changes_reach_the_chips_and_the_next_menu_without_a_read() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on_codex());
        let snapshot = config_snapshot(vec![codex("ready"), claude("ready")], json!({}));
        app.on_config_event(snapshot);
        let chip = model_chip(&mut app);
        assert!(chip.contains("GPT-6"), "{chip}");
        assert!(!chip.contains("Turbo"), "{chip}");

        // With no menu open, T3 renames the thread's model and finds Claude broken.
        let models = [("gpt-6", "GPT-6 Turbo"), ("gpt-6-mini", "GPT-6 Mini")];
        let renamed = provider("codex", "Codex", "ready", &models);
        app.on_config_event(provider_statuses(vec![renamed, claude("error")]));
        let chip = model_chip(&mut app);
        assert!(chip.contains("GPT-6 Turbo"), "{chip}");

        // The next menu shows both changes from the copy the TUI holds, and reads nothing.
        alt(&mut app, 'm');
        let menu = menu_text(&mut app);
        assert!(menu.contains("GPT-6 Turbo"), "{menu}");
        assert!(menu.contains("Claude (error)"), "{menu}");
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn an_open_menu_keeps_its_highlight_on_the_same_model_as_providers_change() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on_codex());
        let snapshot = config_snapshot(vec![codex("ready"), claude("ready")], json!({}));
        app.on_config_event(snapshot);
        alt(&mut app, 'm');
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
        }
        let haiku = Some(Pick::Model("claudeAgent/claude-haiku-4-5".into()));
        assert_eq!(app.selected_pick(), haiku);
        let row = app.picker.as_ref().unwrap().selected;

        // A provider listed first moves Haiku two rows down, and the highlight moves with it.
        let models = [("composer-2", "Composer 2")];
        let cursor = provider("cursor", "Cursor", "ready", &models);
        let providers = vec![cursor.clone(), codex("ready"), claude("ready")];
        app.on_config_event(provider_statuses(providers));
        let menu = menu_text(&mut app);
        assert!(menu.contains("Composer 2"), "{menu}");
        assert_eq!(app.selected_pick(), haiku);
        assert_eq!(app.picker.as_ref().unwrap().selected, row + 2);

        // With Claude gone, the highlight goes to the thread's model and the menu stays open.
        app.on_config_event(provider_statuses(vec![cursor, codex("ready")]));
        let gpt = Some(Pick::Model("codex/gpt-6".into()));
        assert_eq!(app.selected_pick(), gpt);
        let menu = menu_text(&mut app);
        assert!(!menu.contains("Haiku"), "{menu}");
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn provider_changes_leave_the_draft_and_the_composer_alone() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on_codex());
        let snapshot = config_snapshot(vec![codex("ready"), claude("ready")], json!({}));
        app.on_config_event(snapshot);
        app.composer.insert_str("half a reply");
        alt(&mut app, 'm');
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Enter);
        let haiku = Some("claudeAgent/claude-haiku-4-5".to_string());
        assert_eq!(app.drafts["t"].model, haiku);
        let chip = model_chip(&mut app);
        assert!(chip.contains("Haiku 4.5"), "{chip}");

        // While Claude is gone, the chip falls back to the thread's model. The draft waits for
        // Claude to come back.
        app.on_config_event(provider_statuses(vec![codex("ready")]));
        let chip = model_chip(&mut app);
        assert!(chip.contains("GPT-6"), "{chip}");
        assert_eq!(app.drafts["t"].model, haiku);
        assert_eq!(app.composer.text(), "half a reply");

        app.on_config_event(provider_statuses(vec![codex("ready"), claude("ready")]));
        let chip = model_chip(&mut app);
        assert!(chip.contains("Haiku 4.5"), "{chip}");
        assert_eq!(app.composer.text(), "half a reply");
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn a_model_list_read_that_answers_after_live_config_puts_nothing_back() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on_codex());
        // With no configuration yet, the menu reads it.
        alt(&mut app, 'm');
        assert_eq!(reads(&runtime), 1);
        let menu = menu_text(&mut app);
        assert!(menu.contains("Loading T3's model list…"), "{menu}");
        let asked_at = app.config_revision;

        // The subscription's snapshot arrives first and finds Claude broken.
        let snapshot = config_snapshot(vec![codex("ready"), claude("error")], json!({}));
        app.on_config_event(snapshot);
        let menu = menu_text(&mut app);
        assert!(menu.contains("Claude (error)"), "{menu}");

        // The read answers late with an older list, which changes nothing.
        let pi = provider("pi", "Pi", "ready", &[("pi-1", "Pi One")]);
        let older = json!({"providers": [codex("ready"), claude("ready"), pi]});
        let answer = ActionResult::Config {
            config: older,
            revision: asked_at,
        };
        app.on_action_result(answer);
        let menu = menu_text(&mut app);
        assert!(menu.contains("Claude (error)"), "{menu}");
        assert!(!menu.contains("Pi One"), "{menu}");

        // Later changes still apply, and the next menu reads nothing.
        app.on_config_event(provider_statuses(vec![codex("ready"), claude("ready")]));
        press(&mut app, KeyCode::Esc);
        alt(&mut app, 'm');
        let menu = menu_text(&mut app);
        assert!(menu.contains("Opus 5.5"), "{menu}");
        assert!(!menu.contains("(error)"), "{menu}");
        assert_eq!(reads(&runtime), 1);
    }

    #[test]
    fn a_model_list_read_fills_the_menu_until_the_first_snapshot() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on_codex());
        alt(&mut app, 'm');
        assert_eq!(reads(&runtime), 1);
        let asked_at = app.config_revision;

        // A change before any snapshot has nothing to apply to, so the read's answer still
        // counts.
        app.on_config_event(provider_statuses(vec![codex("error")]));
        let menu = menu_text(&mut app);
        assert!(menu.contains("Loading T3's model list…"), "{menu}");
        let config = json!({"providers": [codex("ready"), claude("ready")]});
        let answer = ActionResult::Config {
            config,
            revision: asked_at,
        };
        app.on_action_result(answer);
        let menu = menu_text(&mut app);
        assert!(menu.contains("Opus 5.5"), "{menu}");
        assert!(!menu.contains("(error)"), "{menu}");

        // The first snapshot replaces what the read brought.
        app.on_config_event(config_snapshot(vec![codex("ready")], json!({})));
        let menu = menu_text(&mut app);
        assert!(menu.contains("GPT-6"), "{menu}");
        assert!(!menu.contains("Opus 5.5"), "{menu}");
        assert_eq!(reads(&runtime), 1);
    }

    #[test]
    fn a_reconnect_keeps_the_config_until_a_fresh_snapshot_replaces_it() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        let mut zulu = listed("z", "Zulu", "idle");
        zulu["snoozedUntil"] = json!("2999-01-01T00:00:00.000Z");
        app.on_shell_event(shell_snapshot(vec![zulu]));
        app.open_thread(thread_on_codex());
        let rows = screen(&mut app, 120, 30);
        assert!(!rows.iter().any(|row| row.contains("Snoozed")), "{rows:#?}");

        // The first snapshot turns snooze on, and the sidebar moves Zulu to its shelf.
        let providers = vec![codex("ready"), claude("ready")];
        let snapshot = config_snapshot(providers, json!({"threadSnooze": true}));
        app.on_config_event(snapshot);
        let rows = screen(&mut app, 120, 30);
        let snoozed = row_showing(&rows, "Snoozed ─");
        assert!(snoozed < row_showing(&rows, "Zulu"));
        alt(&mut app, 'm');
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
        }
        let haiku = Some(Pick::Model("claudeAgent/claude-haiku-4-5".into()));
        assert_eq!(app.selected_pick(), haiku);

        // While the subscription reconnects, everything stays as the last snapshot left it.
        let reason = "T3 connection lost: socket ended".to_string();
        let retry_in = Duration::from_millis(250);
        app.on_config_event(WatchEvent::Reconnecting { reason, retry_in });
        assert_eq!(app.selected_pick(), haiku);
        let menu = menu_text(&mut app);
        assert!(menu.contains("Haiku 4.5"), "{menu}");
        let rows = screen(&mut app, 120, 30);
        let snoozed = row_showing(&rows, "Snoozed ─");
        assert!(snoozed < row_showing(&rows, "Zulu"));

        // The new subscription's snapshot replaces all of it, so Claude and snooze go.
        app.on_config_event(config_snapshot(vec![codex("ready")], json!({})));
        let gpt = Some(Pick::Model("codex/gpt-6".into()));
        assert_eq!(app.selected_pick(), gpt);
        let rows = screen(&mut app, 120, 30);
        assert!(!rows.iter().any(|row| row.contains("Snoozed")), "{rows:#?}");
        assert!(row_showing(&rows, "Working (1)") < row_showing(&rows, "Zulu"));
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn after_t3_refuses_the_config_subscription_each_menu_reads_the_model_list() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on_codex());
        // T3 refuses the subscription, and the TUI reads the configuration once at once.
        let refusal = "Unknown request tag: subscribeServerConfig".to_string();
        app.on_config_event(WatchEvent::Failed(refusal));
        assert_eq!(reads(&runtime), 1);
        let asked_at = app.config_revision;
        let config = json!({"providers": [codex("ready"), claude("ready")]});
        let answer = ActionResult::Config {
            config,
            revision: asked_at,
        };
        app.on_action_result(answer);

        // Each menu now reads it again, and shows the last copy until the answer comes.
        alt(&mut app, 'm');
        assert_eq!(reads(&runtime), 2);
        let menu = menu_text(&mut app);
        assert!(menu.contains("Opus 5.5"), "{menu}");
        assert!(!menu.contains("(error)"), "{menu}");
        let config = json!({"providers": [codex("ready"), claude("error")]});
        let answer = ActionResult::Config {
            config,
            revision: asked_at,
        };
        app.on_action_result(answer);
        let menu = menu_text(&mut app);
        assert!(menu.contains("Claude (error)"), "{menu}");

        // Moving to another menu reads nothing more. Opening one again reads once more.
        alt(&mut app, 'e');
        assert_eq!(reads(&runtime), 2);
        press(&mut app, KeyCode::Esc);
        alt(&mut app, 'm');
        assert_eq!(reads(&runtime), 3);
    }

    // ---- the provider status banner ----

    /// A provider as the nightly reports it, with GPT-6 and GPT-6 Mini, so the provider banner
    /// reads it. `state` and `auth` say how it is.
    fn reported(id: &str, name: &str, state: &str, auth: &str) -> Value {
        let mut provider = codex(state);
        provider["instanceId"] = json!(id);
        provider["displayName"] = json!(name);
        provider["driver"] = json!("codex");
        provider["installed"] = json!(true);
        provider["auth"] = json!({"status": auth});
        provider["checkedAt"] = json!("2026-10-10T12:00:00.000Z");
        provider
    }

    /// `provider` with `message`.
    fn saying(mut provider: Value, message: &str) -> Value {
        provider["message"] = json!(message);
        provider
    }

    /// Thread `id`, open and live on `instance`'s GPT-6 as `codex_thread` has it, with the
    /// Codex session's `last_error` and `more` in its projection.
    fn thread_on(id: &str, instance: &str, last_error: Option<&str>, more: Value) -> OpenThread {
        let mut more = more;
        more["thread"] = json!({
            "id": id,
            "providerInstanceId": instance,
            "activeProviderThreadId": "pt",
            "modelSelection": {"instanceId": instance, "model": "gpt-6"},
        });
        codex_thread(id, last_error, more)
    }

    /// The provider banner's text in a frame 120 columns by 30 rows, empty when it drew none.
    fn provider_text(app: &mut App) -> String {
        let buffer = cells(app, 120, 30);
        text_in(&buffer, app.provider_banner.area)
    }

    /// How many times the provider banner followed a selection worked out again, and cleaned a
    /// notice it didn't have.
    fn provider_work(app: &App) -> (usize, usize) {
        (app.provider_banner.selects, app.provider_banner.prepares)
    }

    #[test]
    fn the_provider_banner_stacks_over_the_error_and_each_dismisses_alone() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on("t", "codex", None, json!({})));
        let ready = reported("codex", "Codex", "ready", "authenticated");
        app.on_config_event(config_snapshot(vec![ready], json!({})));
        app.composer.insert_str("half a reply");
        app.focus = Focus::Transcript;
        app.scroll.set(12);
        let before = cells(&mut app, 120, 30);
        assert_eq!(
            app.provider_banner.area,
            Rect::default(),
            "ready, so no notice"
        );
        let (anchor, body) = (app.anchor.clone(), app.transcript_area);

        // Codex fails to start, and the thread's session fails with it.
        let failing = reported("codex", "Codex", "error", "authenticated");
        app.on_config_event(provider_statuses(vec![saying(
            failing,
            "Codex can't start.",
        )]));
        app.on_thread_event(session_error(2, json!("Codex quit")));
        let after = cells(&mut app, 120, 30);
        let (provider, error) = (app.provider_banner.area, app.banner.area);
        assert_eq!(provider.y, body.y, "the provider banner is on top");
        assert_eq!(error.y, provider.bottom(), "the error banner is under it");
        assert_eq!(provider.intersection(body), provider);
        assert_eq!(error.intersection(body), error);
        assert!(provider.height + error.height <= body.height / 2);
        let text = text_in(&after, provider);
        assert!(text.contains("i Codex provider status"), "{text}");
        assert!(text.contains("Codex can't start."), "{text}");
        assert!(text.contains("Alt+N dismiss"), "{text}");
        assert_eq!(after[(provider.x, provider.y)].fg, app.theme.error);
        let text = text_in(&after, error);
        assert!(text.contains("! Codex quit"), "{text}");
        assert!(text.contains("Alt+W dismiss"), "{text}");
        // The rest of the transcript is as it was, and the reader is where they were.
        for y in body.y..body.bottom() {
            for x in body.x..body.right() {
                let at = ratatui::layout::Position::new(x, y);
                if !provider.contains(at) && !error.contains(at) {
                    assert_eq!(after[(x, y)], before[(x, y)], "({x}, {y})");
                }
            }
        }
        assert_eq!(app.anchor, anchor);
        assert_eq!(app.scroll.rows(), 12);

        // Alt+W dismisses the error and leaves the provider's notice.
        alt(&mut app, 'w');
        screen(&mut app, 120, 30);
        assert!(app.provider_banner.showing());
        assert!(!app.banner.showing());
        // Another error comes, and Alt+N dismisses the notice alone. The error moves up.
        app.on_thread_event(session_error(3, json!("Codex quit again")));
        screen(&mut app, 120, 30);
        alt(&mut app, 'n');
        screen(&mut app, 120, 30);
        assert!(!app.provider_banner.showing());
        assert!(app.banner.showing());
        assert_eq!(app.banner.area.y, body.y);
        assert_eq!(app.composer.text(), "half a reply");
        assert_eq!(app.focus, Focus::Transcript);
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn the_banner_follows_the_instance_the_composer_would_send_with() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on("t", "codex", None, json!({})));
        // Two instances of the same driver. Only the work one has a problem.
        let personal = reported("codex", "Codex", "ready", "authenticated");
        let work = reported("codex_work", "Codex work", "warning", "authenticated");
        let work = saying(work, "Rate limited");
        app.on_config_event(config_snapshot(vec![personal.clone(), work], json!({})));
        assert_eq!(
            provider_text(&mut app),
            "",
            "the thread is on the personal instance"
        );

        // A change to an instance the composer doesn't use shows nothing.
        let worse = reported("codex_work", "Codex work", "error", "unauthenticated");
        let worse = saying(worse, "Signed out");
        app.on_config_event(provider_statuses(vec![personal.clone(), worse.clone()]));
        assert_eq!(provider_text(&mut app), "");

        // An unsent choice of the work instance's GPT-6 shows that instance's notice.
        alt(&mut app, 'm');
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        let work_gpt = Some(Pick::Model("codex_work/gpt-6".into()));
        assert_eq!(app.selected_pick(), work_gpt);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.drafts["t"].model.as_deref(), Some("codex_work/gpt-6"));
        let text = provider_text(&mut app);
        assert!(text.contains("Codex work is unauthenticated"), "{text}");
        assert!(text.contains("Signed out"), "{text}");

        // The work instance recovers, and the banner goes. It fails again, and it comes back.
        let recovered = reported("codex_work", "Codex work", "ready", "authenticated");
        app.on_config_event(provider_statuses(vec![personal.clone(), recovered]));
        assert_eq!(provider_text(&mut app), "");
        app.on_config_event(provider_statuses(vec![personal.clone(), worse.clone()]));
        assert!(provider_text(&mut app).contains("Signed out"));

        // With no models left, the choice can't be sent, so the chip and the banner go back to
        // the thread's instance. The choice waits for the models to come back.
        let mut emptied = worse.clone();
        emptied["models"] = json!([]);
        app.on_config_event(provider_statuses(vec![personal.clone(), emptied]));
        assert_eq!(provider_text(&mut app), "");
        assert_eq!(app.drafts["t"].model.as_deref(), Some("codex_work/gpt-6"));
        app.on_config_event(provider_statuses(vec![personal.clone(), worse]));
        assert!(provider_text(&mut app).contains("Signed out"));

        // Choosing the thread's own model again drops the choice, and the banner with it.
        alt(&mut app, 'm');
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.selected_pick(), Some(Pick::Model("codex/gpt-6".into())));
        press(&mut app, KeyCode::Enter);
        assert!(!app.drafts.contains_key("t"));
        assert_eq!(provider_text(&mut app), "");
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn one_dismissal_holds_across_threads_until_a_selection_has_no_notice() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        let warning = reported("codex", "Codex", "warning", "authenticated");
        let warning = saying(warning, "Degraded");
        let work = reported("codex_work", "Codex work", "error", "authenticated");
        let spare = reported("codex_spare", "Codex spare", "ready", "authenticated");
        app.on_config_event(config_snapshot(vec![warning, work, spare], json!({})));
        assert_eq!(provider_text(&mut app), "", "no thread, no banner");
        app.open_thread(thread_on("a", "codex", None, json!({})));
        assert!(provider_text(&mut app).contains("Degraded"));
        alt(&mut app, 'n');
        assert_eq!(provider_text(&mut app), "");

        // Another thread on the same instance has the same notice, so it stays dismissed.
        app.open_thread(thread_on("b", "codex", None, json!({})));
        assert_eq!(provider_text(&mut app), "");
        // A thread on another instance shows that one's notice. The first stays dismissed.
        app.open_thread(thread_on("c", "codex_work", None, json!({})));
        let text = provider_text(&mut app);
        assert!(text.contains("Codex work provider status"), "{text}");
        assert!(
            text.contains("Codex work provider is unavailable."),
            "{text}"
        );
        app.open_thread(thread_on("a", "codex", None, json!({})));
        assert_eq!(provider_text(&mut app), "");
        // A thread on a ready instance has no notice, which ends the dismissal.
        app.open_thread(thread_on("d", "codex_spare", None, json!({})));
        assert_eq!(provider_text(&mut app), "");
        app.open_thread(thread_on("a", "codex", None, json!({})));
        assert!(provider_text(&mut app).contains("Degraded"));
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn the_banner_finds_the_threads_instance_without_models_and_after_it_leaves() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on("t", "codex", None, json!({})));
        let ready = reported("codex", "Codex", "ready", "authenticated");
        let other = reported("codex_work", "Codex work", "warning", "authenticated");
        let other = saying(other, "Work is degraded");
        app.on_config_event(config_snapshot(
            vec![ready.clone(), other.clone()],
            json!({}),
        ));
        assert_eq!(provider_text(&mut app), "");

        // Codex loses its models and warns. The banner still finds it by its instance id.
        let empty = reported("codex", "Codex", "warning", "authenticated");
        let mut empty = saying(empty, "No models found");
        empty["models"] = json!([]);
        app.on_config_event(provider_statuses(vec![empty.clone(), other.clone()]));
        let text = provider_text(&mut app);
        assert!(text.contains("Codex provider status"), "{text}");
        assert!(text.contains("No models found"), "{text}");
        alt(&mut app, 'n');
        assert_eq!(provider_text(&mut app), "");

        // T3 stops listing Codex. No other instance stands in for it, and the dismissal ends.
        app.on_config_event(provider_statuses(vec![other.clone()]));
        assert_eq!(provider_text(&mut app), "");
        // Codex comes back with the same notice, which shows again, and then recovers.
        app.on_config_event(provider_statuses(vec![empty, other.clone()]));
        assert!(provider_text(&mut app).contains("No models found"));
        app.on_config_event(provider_statuses(vec![ready, other]));
        assert_eq!(provider_text(&mut app), "");

        // Another client moves the thread to the work instance, and its notice shows.
        let moved = json!({
            "id": "t",
            "providerInstanceId": "codex_work",
            "activeProviderThreadId": "pt",
            "modelSelection": {"instanceId": "codex_work", "model": "gpt-6"},
        });
        app.on_thread_event(live(2, "thread.updated", moved));
        assert!(provider_text(&mut app).contains("Work is degraded"));
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn streaming_frames_and_unchanged_statuses_do_no_provider_work() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on("t", "codex", None, json!({})));
        let warning = reported("codex", "Codex", "warning", "authenticated");
        let mut warning = saying(warning, "Degraded");
        app.on_config_event(config_snapshot(vec![warning.clone()], json!({})));
        screen(&mut app, 120, 30);
        assert!(app.provider_banner.showing());
        let start = provider_work(&app);

        // A run starts, a hundred pieces of an answer stream with a frame after each, a message
        // lands, the watch reconnects, and the reader opens the notice. None of it looks at the
        // provider.
        let run = json!({"id": "r1", "ordinal": 1, "status": "running", "rootNodeId": "r1-n"});
        app.on_thread_event(live(2, "run.created", run));
        stream(&mut app, 3, 100);
        let message = json!({"id": "m9", "role": "user", "text": "hello"});
        app.on_thread_event(live(103, "message.updated", message));
        app.on_thread_event(WatchEvent::Reconnecting {
            reason: "socket closed".into(),
            retry_in: Duration::from_secs(1),
        });
        alt(&mut app, 'o');
        screen(&mut app, 120, 30);
        assert!(app.provider_banner.expanded());
        assert_eq!(provider_work(&app), start);

        // A thread event that keeps the model selection only compares it.
        let renamed = json!({
            "id": "t",
            "title": "Renamed",
            "providerInstanceId": "codex",
            "activeProviderThreadId": "pt",
            "modelSelection": {"instanceId": "codex", "model": "gpt-6"},
        });
        app.on_thread_event(live(104, "thread.metadata-updated", renamed));
        assert_eq!(provider_work(&app), start);

        // T3 checks Codex again every few seconds and finds it as it was. Each check works the
        // selection out again and compares the notice, and the open banner stays open.
        for second in 1..=5 {
            warning["checkedAt"] = json!(format!("2026-10-10T12:00:0{second}.000Z"));
            app.on_config_event(provider_statuses(vec![warning.clone()]));
            screen(&mut app, 120, 30);
        }
        assert_eq!(provider_work(&app), (start.0 + 5, start.1));
        assert!(app.provider_banner.expanded());
        // A new message is a new notice, cleaned once, and it starts closed.
        warning["message"] = json!("Degraded again");
        app.on_config_event(provider_statuses(vec![warning]));
        assert_eq!(provider_work(&app), (start.0 + 6, start.1 + 1));
        assert!(!app.provider_banner.expanded());
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn one_banner_is_open_at_a_time_and_alt_arrows_scroll_the_open_one_first() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        let lines = |what: &str| {
            (1..=40)
                .map(|n| format!("{what} line {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let request = json!({"id": "q1", "kind": "command", "status": "pending"});
        let more = json!({"runtimeRequests": [request]});
        app.open_thread(thread_on("t", "codex", Some(&lines("error")), more));
        let failing = reported("codex", "Codex", "error", "authenticated");
        let failing = saying(failing, &lines("notice"));
        app.on_config_event(config_snapshot(vec![failing], json!({})));
        app.composer.insert_str("half a reply");
        screen(&mut app, 120, 40);
        assert!(app.provider_banner.showing() && app.banner.showing());
        assert!(app.panel_area.height > 0);
        let down = || Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));

        // Alt+O opens the notice, which takes Alt+↓ first.
        alt(&mut app, 'o');
        screen(&mut app, 120, 40);
        assert!(app.provider_banner.expanded());
        app.on_terminal_event(down());
        let buffer = cells(&mut app, 120, 40);
        let text = text_in(&buffer, app.provider_banner.area);
        assert!(text.contains("Lines 2-"), "{text}");
        assert_eq!(app.panel_scroll, 0);

        // Alt+I opens the error and closes the notice, and Alt+↓ scrolls the error.
        alt(&mut app, 'i');
        assert!(app.banner.expanded() && !app.provider_banner.expanded());
        screen(&mut app, 120, 40);
        app.on_terminal_event(down());
        let buffer = cells(&mut app, 120, 40);
        let text = text_in(&buffer, app.banner.area);
        assert!(text.contains("Lines 2-"), "{text}");
        assert_eq!(app.panel_scroll, 0);

        // A click on the notice opens it and closes the error.
        let area = app.provider_banner.area;
        assert!(mouse(&mut app, CLICK, area.x + 2, area.y + 1));
        assert!(app.provider_banner.expanded() && !app.banner.expanded());

        // With both closed, Alt+↓ reaches the request panel.
        alt(&mut app, 'o');
        screen(&mut app, 120, 40);
        assert!(!app.provider_banner.expanded() && !app.banner.expanded());
        app.on_terminal_event(down());
        assert_eq!(app.panel_scroll, 1);

        // A click on the notice's × dismisses it alone, and nothing reaches the composer.
        let close = app.provider_banner.close;
        assert!(mouse(&mut app, CLICK, close.x + 1, close.y));
        screen(&mut app, 120, 40);
        assert!(!app.provider_banner.showing() && app.banner.showing());
        assert_eq!(app.composer.text(), "half a reply");
        assert_eq!(reads(&runtime), 0);
    }

    /// Forty lines of `what`, more than a banner shows on a screen 40 rows tall.
    fn forty_lines(what: &str) -> String {
        (1..=40)
            .map(|n| format!("{what} line {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn alt_arrow(app: &mut App, code: KeyCode) {
        app.on_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::ALT)));
    }

    #[test]
    fn a_new_notice_over_an_open_one_hands_alt_arrows_to_the_request_panel_before_a_frame() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        let request = json!({"id": "q1", "kind": "command", "status": "pending"});
        let more = json!({"runtimeRequests": [request]});
        app.open_thread(thread_on("t", "codex", None, more));
        let failing = reported("codex", "Codex", "error", "authenticated");
        let mut failing = saying(failing, &forty_lines("notice"));
        app.on_config_event(config_snapshot(vec![failing.clone()], json!({})));
        app.composer.insert_str("half a reply");
        app.focus = Focus::Transcript;
        app.scroll.set(12);
        screen(&mut app, 120, 40);
        assert!(app.provider_banner.showing() && !app.banner.showing());
        assert!(app.panel_area.height > 0);

        // The reader opens the notice, a frame draws it too long for its rows, and Alt+↓
        // scrolls it. T3 checks Codex again and finds it as it was, and before the next frame
        // Alt+↓ still scrolls the open notice.
        alt(&mut app, 'o');
        screen(&mut app, 120, 40);
        alt_arrow(&mut app, KeyCode::Down);
        failing["checkedAt"] = json!("2026-10-10T12:00:05.000Z");
        app.on_config_event(provider_statuses(vec![failing.clone()]));
        assert!(app.provider_banner.expanded());
        alt_arrow(&mut app, KeyCode::Down);
        assert_eq!(app.panel_scroll, 0);
        let buffer = cells(&mut app, 120, 40);
        let text = text_in(&buffer, app.provider_banner.area);
        assert!(text.contains("Lines 3-"), "{text}");
        let scrolled = app.scroll.rows();
        assert!(scrolled > 0);

        // Codex's message changes, so a new notice takes its place, closed. The last frame
        // drew the open one, but before the next frame Alt+↓ and Alt+↑ reach the request
        // panel. Nothing else moves, and nothing goes to T3.
        failing["message"] = json!(forty_lines("new notice"));
        app.on_config_event(provider_statuses(vec![failing]));
        assert!(app.provider_banner.showing() && !app.provider_banner.expanded());
        alt_arrow(&mut app, KeyCode::Down);
        assert_eq!(app.panel_scroll, 1);
        alt_arrow(&mut app, KeyCode::Down);
        assert_eq!(app.panel_scroll, 2);
        alt_arrow(&mut app, KeyCode::Up);
        assert_eq!(app.panel_scroll, 1);
        assert_eq!(app.composer.text(), "half a reply");
        assert_eq!(app.focus, Focus::Transcript);
        assert_eq!(app.scroll.rows(), scrolled);

        // The next frame draws the new notice closed, at its first rows.
        let buffer = cells(&mut app, 120, 40);
        let text = text_in(&buffer, app.provider_banner.area);
        assert!(text.contains("new notice line 1"), "{text}");
        assert!(text.contains("Alt+O more"), "{text}");
        assert!(!text.contains("Lines "), "{text}");
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn closing_a_banner_or_a_new_error_hands_on_alt_arrows_and_the_wheel_before_a_frame() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        let request = json!({"id": "q1", "kind": "command", "status": "pending"});
        let more = json!({"runtimeRequests": [request]});
        app.open_thread(thread_on("t", "codex", Some(&forty_lines("error")), more));
        let failing = reported("codex", "Codex", "error", "authenticated");
        let failing = saying(failing, &forty_lines("notice"));
        app.on_config_event(config_snapshot(vec![failing], json!({})));
        app.composer.insert_str("half a reply");
        screen(&mut app, 120, 40);
        assert!(app.provider_banner.showing() && app.banner.showing());
        assert!(app.panel_area.height > 0);

        // Alt+O opens the notice and a frame draws it too long for its rows. Alt+O closes it,
        // and before a frame draws it closed, Alt+↓ reaches the request panel. Alt+I does the
        // same for the error.
        alt(&mut app, 'o');
        screen(&mut app, 120, 40);
        alt(&mut app, 'o');
        let before = app.panel_scroll;
        alt_arrow(&mut app, KeyCode::Down);
        assert_eq!(app.panel_scroll, before + 1);
        screen(&mut app, 120, 40);
        alt(&mut app, 'i');
        screen(&mut app, 120, 40);
        alt(&mut app, 'i');
        let before = app.panel_scroll;
        alt_arrow(&mut app, KeyCode::Down);
        assert_eq!(app.panel_scroll, before + 1);

        // A click closes the open notice, and the wheel where the frame drew it open scrolls
        // the transcript under it, as it does under any closed notice.
        screen(&mut app, 120, 40);
        alt(&mut app, 'o');
        screen(&mut app, 120, 40);
        let area = app.provider_banner.area;
        assert!(mouse(&mut app, CLICK, area.x + 2, area.y + 1));
        assert!(!app.provider_banner.expanded());
        let scrolled = app.scroll.rows();
        let (x, y) = (area.x + 2, area.bottom() - 2);
        assert!(mouse(&mut app, MouseEventKind::ScrollUp, x, y));
        assert_eq!(app.scroll.rows(), scrolled + 3);

        // The same for the error.
        screen(&mut app, 120, 40);
        alt(&mut app, 'i');
        screen(&mut app, 120, 40);
        let area = app.banner.area;
        assert!(mouse(&mut app, CLICK, area.x + 2, area.y + 1));
        assert!(!app.banner.expanded());
        let scrolled = app.scroll.rows();
        let (x, y) = (area.x + 2, area.bottom() - 2);
        assert!(mouse(&mut app, MouseEventKind::ScrollUp, x, y));
        assert_eq!(app.scroll.rows(), scrolled + 3);

        // Codex's session error changes under the open error. The new one starts closed, and
        // before a frame draws it, Alt+↓ reaches the request panel.
        screen(&mut app, 120, 40);
        alt(&mut app, 'i');
        screen(&mut app, 120, 40);
        app.on_thread_event(session_error(2, json!(forty_lines("new error"))));
        assert!(app.banner.showing() && !app.banner.expanded());
        let before = app.panel_scroll;
        alt_arrow(&mut app, KeyCode::Down);
        assert_eq!(app.panel_scroll, before + 1);
        let buffer = cells(&mut app, 120, 40);
        let text = text_in(&buffer, app.banner.area);
        assert!(text.contains("new error line 1"), "{text}");
        assert_eq!(app.composer.text(), "half a reply");
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn at_any_size_the_banners_stay_apart_inside_the_transcript() {
        let long = format!(
            "{}\n\u{1b}[31m{}\u{7}\t👩‍💻 e\u{301}",
            "界".repeat(300),
            "a long line ".repeat(40)
        );
        let name = format!("Codex {}", "界".repeat(50));
        let small = [(0, 0), (1, 1), (10, 5), (19, 3), (26, 4), (40, 8), (40, 12)];
        // Growing, then shrinking, so nothing from a larger frame is left behind.
        let large = [(60, 16), (80, 24), (200, 60), (26, 4), (80, 24)];
        let sizes: Vec<(u16, u16)> = small.into_iter().chain(large).collect();
        for hidden in [false, true] {
            let settings = Settings {
                sidebar_hidden: hidden,
                ..Settings::default()
            };
            let mut app = tui(&settings);
            app.open_thread(thread_on("t", "codex", Some(&long), json!({})));
            let failing = saying(reported("codex", &name, "error", "authenticated"), &long);
            app.on_config_event(config_snapshot(vec![failing], json!({})));
            app.composer.insert_str("a draft that wraps when narrow");
            for open in [None, Some('o'), Some('i')] {
                if let Some(c) = open {
                    screen(&mut app, 120, 30);
                    alt(&mut app, c);
                }
                for &(width, height) in &sizes {
                    let size = format!("{width}x{height}, open: {open:?}, hidden: {hidden}");
                    let buffer = cells(&mut app, width, height);
                    let body = app.transcript_area;
                    let provider = (app.provider_banner.area, app.provider_banner.close);
                    let error = (app.banner.area, app.banner.close);
                    for (area, close) in [provider, error] {
                        if area.is_empty() {
                            assert_eq!(close, Rect::default(), "{size}");
                            continue;
                        }
                        assert_eq!(area.intersection(body), area, "{size}");
                        assert_eq!(close.intersection(area), close, "{size}");
                        assert!(text_in(&buffer, close).contains('×'), "{size}");
                        let printable = |c: char| c == '\n' || !c.is_control();
                        let text = text_in(&buffer, area);
                        assert!(text.chars().all(printable), "{size}: {text:?}");
                    }
                    assert!(!provider.0.intersects(error.0), "{size}");
                    let half = (body.height / 2).max(1);
                    if open.is_none() {
                        assert!(provider.0.height + error.0.height <= half, "{size}");
                    }
                    // A closed notice gives way to the thread's error on a short transcript.
                    if body.height < 8 && open != Some('o') {
                        assert!(provider.0.is_empty(), "{size}");
                    }
                }
            }
            assert_eq!(app.composer.text(), "a draft that wraps when narrow");
        }
    }

    #[test]
    fn a_late_model_list_read_or_a_reconnect_never_brings_an_old_status_back() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui(&Settings::default());
        app.open_thread(thread_on("t", "codex", None, json!({})));
        // With no configuration yet, the menu reads it.
        alt(&mut app, 'm');
        assert_eq!(reads(&runtime), 1);
        let asked_at = app.config_revision;
        press(&mut app, KeyCode::Esc);

        // The subscription's snapshot arrives first and finds Codex signed out.
        let signed_out = reported("codex", "Codex", "error", "unauthenticated");
        app.on_config_event(config_snapshot(vec![signed_out], json!({})));
        let text = provider_text(&mut app);
        assert!(text.contains("Codex is unauthenticated"), "{text}");
        assert!(
            text.contains("Sign in via the CLI to authenticate again."),
            "{text}"
        );

        // The read answers late with Codex ready, which changes nothing.
        let ready = reported("codex", "Codex", "ready", "authenticated");
        let older = json!({"providers": [ready.clone()]});
        let answer = ActionResult::Config {
            config: older,
            revision: asked_at,
        };
        app.on_action_result(answer);
        assert!(provider_text(&mut app).contains("Codex is unauthenticated"));

        // While the subscription reconnects the notice stays, until the new snapshot.
        let reason = "T3 connection lost: socket ended".to_string();
        let retry_in = Duration::from_millis(250);
        app.on_config_event(WatchEvent::Reconnecting { reason, retry_in });
        assert!(provider_text(&mut app).contains("Codex is unauthenticated"));
        app.on_config_event(config_snapshot(vec![ready], json!({})));
        assert_eq!(provider_text(&mut app), "");
        assert_eq!(reads(&runtime), 1);
    }

    // ---- worktree setup ----

    /// Each worktree setup subscription the TUI opened, with the thread it is for and the end
    /// that stands in for T3.
    type Subscriptions = Arc<std::sync::Mutex<Vec<(String, mpsc::UnboundedSender<WatchEvent>)>>>;

    /// The TUI with the sidebar hidden, noting each worktree setup subscription it opens.
    fn setup_tui() -> (App, Subscriptions) {
        let hidden = Settings {
            sidebar_hidden: true,
            ..Settings::default()
        };
        let mut app = tui(&hidden);
        let subscriptions = Subscriptions::default();
        let opened = subscriptions.clone();
        app.watch_setup = Box::new(move |thread_id: &str| {
            let (sender, events) = mpsc::unbounded_channel();
            opened.lock().unwrap().push((thread_id.to_string(), sender));
            events
        });
        (app, subscriptions)
    }

    /// The threads the subscriptions were opened for, in order.
    fn subscribed(subscriptions: &Subscriptions) -> Vec<String> {
        let subscriptions = subscriptions.lock().unwrap();
        subscriptions.iter().map(|(id, _)| id.clone()).collect()
    }

    /// T3's end of the `index`th subscription.
    fn setup_sender(
        subscriptions: &Subscriptions,
        index: usize,
    ) -> mpsc::UnboundedSender<WatchEvent> {
        subscriptions.lock().unwrap()[index].1.clone()
    }

    /// Hands the TUI what the open thread's setup subscription has waiting, as the event loop
    /// does, and says whether any of it needs a frame.
    fn pump(app: &mut App) -> bool {
        let mut dirty = false;
        loop {
            let event = match app.open.as_mut().and_then(|o| o.setup.events.as_mut()) {
                None => return dirty,
                Some(events) => match events.try_recv() {
                    Ok(event) => Some(event),
                    Err(mpsc::error::TryRecvError::Empty) => return dirty,
                    Err(mpsc::error::TryRecvError::Disconnected) => None,
                },
            };
            dirty |= app.on_setup_event(event);
        }
    }

    /// Sends `event` down the newest subscription and lets the TUI take it, and says whether
    /// the TUI wants a frame for it.
    fn deliver(app: &mut App, subscriptions: &Subscriptions, event: WatchEvent) -> bool {
        let newest = subscriptions.lock().unwrap().len() - 1;
        let sender = setup_sender(subscriptions, newest);
        sender.send(event).expect("the subscription is open");
        pump(app)
    }

    /// Thread `thread`'s worktree setup as T3 sends it, with an `Install` script.
    fn setup_item(thread: &str, phase: &str, sequence: u64, stages: Vec<Value>) -> Value {
        json!({"threadId": thread, "phase": phase, "startedAt": "2026-10-07T00:00:00.000Z",
            "endedAt": null, "branch": format!("t3code/{thread}"), "baseRef": "main",
            "worktreePath": format!("/work/repo-{thread}"),
            "setupScript": {"name": "Install", "command": "bun install", "terminalId": "setup-1"},
            "stages": stages, "error": null, "sequence": sequence})
    }

    /// The five stages in T3's order, with `statuses`.
    fn setup_stages(statuses: [&str; 5]) -> Vec<Value> {
        ["fetch", "checkout", "submodules", "setup-script", "agent"]
            .into_iter()
            .zip(statuses)
            .map(|(id, status)| {
                json!({"id": id, "status": status, "startedAt": null, "endedAt": null,
                    "percent": null, "detail": null, "tail": []})
            })
            .collect()
    }

    /// The thread's run, `r1`, with its turn started or not.
    fn setup_run(status: &str, started: bool) -> Value {
        let started = if started {
            json!("2026-10-07T00:00:05.000Z")
        } else {
            Value::Null
        };
        json!({"id": "r1", "status": status, "ordinal": 1, "startedAt": started})
    }

    /// One short answer after the prompt.
    fn short_answer() -> Vec<Value> {
        vec![
            json!({"id": "a0", "type": "assistant_message", "ordinal": 1,
            "text": "Looking at the login page."}),
        ]
    }

    /// A second prompt, which makes the first turn's setup old news.
    fn follow_up_prompt() -> Value {
        json!({"id": "u1", "type": "user_message", "ordinal": 50, "runId": "r2",
            "text": "Now add a test"})
    }

    /// A snapshot of thread `id`: its first prompt and then `answers`, with `runs`, in its
    /// worktree when `worktree`.
    fn worktree_thread(id: &str, worktree: bool, runs: Value, answers: Vec<Value>) -> WatchEvent {
        let prompt = json!({"id": format!("{id}-u0"), "type": "user_message", "ordinal": 0,
            "runId": "r1", "text": "Fix the login page"});
        let items: Vec<Value> = std::iter::once(prompt).chain(answers).collect();
        let path = if worktree {
            json!(format!("/work/repo-{id}"))
        } else {
            Value::Null
        };
        let projection = json!({"thread": {"id": id, "worktreePath": path},
            "turnItems": items, "runs": runs});
        WatchEvent::Item(
            json!({"kind": "snapshot", "snapshotSequence": 1, "projection": projection}),
        )
    }

    /// Opens thread `id` as picking it does, and gives it `snapshot` from its watch.
    fn open_setup_thread(app: &mut App, id: &str, snapshot: WatchEvent) {
        app.open_thread(OpenThread::new(id.into(), mpsc::unbounded_channel().1));
        app.on_thread_event(snapshot);
    }

    /// The setup card's rows as the next frame draws them, or none.
    fn card_lines(app: &App) -> Vec<String> {
        let card = app.open.as_ref().and_then(|open| open.setup.card());
        card.map(|card| text(&card.lines)).unwrap_or_default()
    }

    fn shows(rows: &[String], needle: &str) -> bool {
        rows.iter().any(|row| row.contains(needle))
    }

    #[test]
    fn a_setup_shows_under_the_first_prompt_until_the_turn_takes_over() {
        let (mut app, subscriptions) = setup_tui();
        // The thread's run is preparing a worktree, which has no path yet.
        let runs = json!([setup_run("preparing", false)]);
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", false, runs, short_answer()),
        );
        assert_eq!(subscribed(&subscriptions), ["t"]);
        // T3 has no setup for the thread yet.
        assert!(!deliver(
            &mut app,
            &subscriptions,
            WatchEvent::Item(Value::Null)
        ));
        let rows = screen(&mut app, 80, 40);
        assert!(!shows(&rows, "Setting up worktree"), "{rows:#?}");

        // Progress shows under the prompt and before the answer.
        let mut stages = setup_stages(["done", "running", "pending", "pending", "pending"]);
        stages[1]["percent"] = json!(42);
        let progress = setup_item("t", "running", 1, stages);
        assert!(deliver(
            &mut app,
            &subscriptions,
            WatchEvent::Item(progress)
        ));
        let rows = screen(&mut app, 80, 40);
        let prompt = row_showing(&rows, "Fix the login page");
        let title = row_showing(&rows, "Setting up worktree…");
        assert!(prompt < title && title < row_showing(&rows, "Looking at the login page"));
        assert!(rows[row_showing(&rows, "Check out files") as usize].contains("42%"));

        // T3 records the worktree's path and the script starts. The agent's stage is done
        // before the turn has started, and in that gap the whole card stays.
        let thread = json!({"id": "t", "worktreePath": "/work/repo-t"});
        app.on_thread_event(live(2, "thread.updated", thread));
        let mut stages = setup_stages(["done", "done", "skipped", "running", "done"]);
        stages[3]["tail"] = json!(["Resolving packages", "Linking dependencies"]);
        let script = setup_item("t", "running", 2, stages);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(script)));
        let rows = screen(&mut app, 80, 40);
        assert_eq!(row_showing(&rows, "Setting up worktree…"), title);
        assert!(shows(&rows, "│ Linking dependencies"));

        // The turn starts, and the script running on is one row with a Details row.
        app.on_thread_event(live(3, "run.updated", setup_run("running", true)));
        let rows = screen(&mut app, 80, 40);
        assert!(!shows(&rows, "Setting up worktree"));
        assert!(!shows(&rows, "Linking dependencies"));
        let background = row_showing(&rows, "Install is still running");
        assert!(prompt < background);
        assert!(rows[background as usize + 1].contains("› Details"));
        // `w` opens the stages and the script's output under that row.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('w'));
        let rows = screen(&mut app, 80, 40);
        let details = row_showing(&rows, "⌄ Details");
        assert_eq!(details, background + 1);
        assert!(row_showing(&rows, "Fetch base branch") > details);
        assert!(shows(&rows, "│ Linking dependencies"));

        // The script finishes cleanly, and with the turn live the card retires.
        let ready = setup_stages(["done", "done", "skipped", "done", "done"]);
        let done = setup_item("t", "done", 3, ready);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(done)));
        let rows = screen(&mut app, 80, 40);
        assert!(!shows(&rows, "Details") && !shows(&rows, "Worktree ready"));
        assert!(card_lines(&app).is_empty());
        assert_eq!(subscribed(&subscriptions), ["t"]);
    }

    #[test]
    fn a_failed_or_cancelled_setup_stays_for_the_first_turn() {
        for (phase, header) in [
            ("failed", "Worktree setup failed"),
            ("cancelled", "Worktree setup cancelled"),
        ] {
            let (mut app, subscriptions) = setup_tui();
            let runs = json!([setup_run("preparing", false)]);
            open_setup_thread(
                &mut app,
                "t",
                worktree_thread("t", true, runs, short_answer()),
            );
            let stages = setup_stages(["done", "failed", "pending", "pending", "pending"]);
            let mut item = setup_item("t", phase, 4, stages);
            item["error"] = json!("git worktree add failed: t3code/t is already checked out");
            assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
            let rows = screen(&mut app, 80, 40);
            assert!(row_showing(&rows, header) > row_showing(&rows, "Fix the login page"));
            // Only a failed setup shows its error, as the desktop's card does.
            assert_eq!(
                shows(&rows, "already checked out"),
                phase == "failed",
                "{phase}"
            );
            // The agent never started, so the whole card stays once the turn is live.
            app.on_thread_event(live(2, "run.updated", setup_run("running", true)));
            let rows = screen(&mut app, 80, 40);
            assert!(shows(&rows, header), "{phase}");
            assert!(shows(&rows, "✕ Check out files"));
            // A follow-up retires it.
            app.on_thread_event(live(3, "turn-item.updated", follow_up_prompt()));
            let rows = screen(&mut app, 80, 40);
            assert!(!shows(&rows, header), "{phase}");
            assert!(shows(&rows, "Now add a test"));
        }
    }

    #[test]
    fn a_failed_setup_script_is_one_row_once_the_turn_is_live() {
        let (mut app, subscriptions) = setup_tui();
        let runs = json!([setup_run("preparing", false)]);
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", true, runs, short_answer()),
        );
        let mut stages = setup_stages(["done", "done", "skipped", "failed", "done"]);
        stages[3]["tail"] = json!(["error: lockfile is out of date"]);
        let item = setup_item("t", "done", 5, stages);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        // Before the turn, the whole card with the script's last output.
        let rows = screen(&mut app, 80, 40);
        assert!(shows(&rows, "Worktree ready, setup script failed"));
        assert!(shows(&rows, "│ error: lockfile is out of date"));
        // During the first turn, one row that says how it went, with its Details row.
        app.on_thread_event(live(2, "run.updated", setup_run("running", true)));
        let rows = screen(&mut app, 80, 40);
        let summary = row_showing(&rows, "✕ Worktree ready, setup script failed");
        assert!(!shows(&rows, "lockfile") && !shows(&rows, "Check out files"));
        assert!(rows[summary as usize + 1].contains("› Details"));
        // After a follow-up, nothing.
        app.on_thread_event(live(3, "turn-item.updated", follow_up_prompt()));
        let rows = screen(&mut app, 80, 40);
        assert!(!shows(&rows, "setup script failed"));
        assert!(card_lines(&app).is_empty());
    }

    #[test]
    fn a_new_setup_after_null_or_a_reconnect_replaces_the_held_one() {
        let (mut app, subscriptions) = setup_tui();
        let runs = json!([setup_run("preparing", false)]);
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", true, runs, short_answer()),
        );
        let ready = || setup_stages(["done", "done", "skipped", "done", "done"]);
        let running = |percent: u64| {
            let mut stages = setup_stages(["done", "running", "pending", "pending", "pending"]);
            stages[1]["percent"] = json!(percent);
            stages
        };
        let send = |app: &mut App, phase: &str, sequence: u64, stages: Vec<Value>| {
            let item = setup_item("t", phase, sequence, stages);
            deliver(app, &subscriptions, WatchEvent::Item(item))
        };
        assert!(send(&mut app, "done", 7, ready()));
        // The same setup again, and an older one in the same subscription, change nothing.
        assert!(!send(&mut app, "done", 7, ready()));
        assert!(!send(&mut app, "running", 6, running(50)));
        let rows = screen(&mut app, 80, 40);
        assert!(shows(&rows, "Worktree ready") && !shows(&rows, "50%"));
        // Thirty seconds after a setup ends T3 forgets it and sends null. The card stays.
        assert!(!deliver(
            &mut app,
            &subscriptions,
            WatchEvent::Item(Value::Null)
        ));
        let rows = screen(&mut app, 80, 40);
        assert!(shows(&rows, "Worktree ready"));
        // T3 counts from zero again after that, so a new setup with a lower sequence shows.
        assert!(send(&mut app, "running", 0, running(10)));
        let rows = screen(&mut app, 80, 40);
        assert!(shows(&rows, "Setting up worktree…") && shows(&rows, "10%"));
        assert!(!shows(&rows, "Worktree ready"));
        assert!(send(&mut app, "running", 3, running(30)));
        assert!(!send(&mut app, "running", 2, running(20)));
        // A reconnect may meet a T3 that restarted and counts from zero too.
        let reconnecting = WatchEvent::Reconnecting {
            reason: "connection closed".into(),
            retry_in: Duration::from_millis(250),
        };
        assert!(!deliver(&mut app, &subscriptions, reconnecting));
        assert!(send(&mut app, "running", 1, running(60)));
        let rows = screen(&mut app, 80, 40);
        assert!(shows(&rows, "60%") && !shows(&rows, "30%"));
        assert_eq!(subscribed(&subscriptions), ["t"]);
    }

    #[test]
    fn each_thread_has_its_own_subscription_and_takes_no_other_threads_setup() {
        let (mut app, subscriptions) = setup_tui();
        let running = || setup_stages(["running", "pending", "pending", "pending", "pending"]);
        open_setup_thread(
            &mut app,
            "a",
            worktree_thread("a", true, json!([]), short_answer()),
        );
        let a = setup_item("a", "running", 1, running());
        assert!(deliver(
            &mut app,
            &subscriptions,
            WatchEvent::Item(a.clone())
        ));
        assert!(shows(&screen(&mut app, 80, 40), "Setting up worktree…"));
        // A's next update is on its way when B opens. B's subscription replaces A's, and A's
        // goes with A, update and all.
        setup_sender(&subscriptions, 0)
            .send(WatchEvent::Item(setup_item("a", "running", 2, running())))
            .expect("A's subscription is open");
        open_setup_thread(
            &mut app,
            "b",
            worktree_thread("b", true, json!([]), short_answer()),
        );
        assert_eq!(subscribed(&subscriptions), ["a", "b"]);
        assert!(setup_sender(&subscriptions, 0).is_closed());
        assert!(!pump(&mut app));
        assert!(!shows(&screen(&mut app, 80, 40), "Setting up worktree"));
        // A's setup sent on B's subscription is not B's.
        assert!(!deliver(&mut app, &subscriptions, WatchEvent::Item(a)));
        assert!(!shows(&screen(&mut app, 80, 40), "Setting up worktree"));
        assert!(card_lines(&app).is_empty());
        // Back on A, a fresh subscription and nothing held from before.
        open_setup_thread(
            &mut app,
            "a",
            worktree_thread("a", true, json!([]), short_answer()),
        );
        assert_eq!(subscribed(&subscriptions), ["a", "b", "a"]);
        assert!(setup_sender(&subscriptions, 1).is_closed());
        assert!(!shows(&screen(&mut app, 80, 40), "Setting up worktree"));
    }

    #[test]
    fn a_refused_subscription_is_not_asked_for_again_and_the_thread_works_on() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let (mut app, subscriptions) = setup_tui();
        open_setup_thread(
            &mut app,
            "a",
            worktree_thread("a", true, json!([]), short_answer()),
        );
        let refused = WatchEvent::Failed("unknown method: subscribeWorktreeSetup".into());
        assert!(!deliver(&mut app, &subscriptions, refused));
        // The card only reads, so a refusal says nothing.
        assert!(app.message.is_none());
        assert!(setup_sender(&subscriptions, 0).is_closed());
        // T3 refuses it for the session, so neither this thread nor another asks again.
        let thread = json!({"id": "a", "worktreePath": "/work/repo-a", "title": "Renamed"});
        app.on_thread_event(live(2, "thread.updated", thread));
        app.on_thread_event(live(3, "run.created", setup_run("preparing", false)));
        open_setup_thread(
            &mut app,
            "b",
            worktree_thread("b", true, json!([]), short_answer()),
        );
        assert_eq!(subscribed(&subscriptions), ["a"]);
        // The thread works without it.
        let answer = json!({"id": "a1", "type": "assistant_message", "ordinal": 2,
            "text": "Still answering."});
        app.on_thread_event(live(2, "turn-item.updated", answer));
        assert!(shows(&screen(&mut app, 80, 40), "Still answering."));
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn a_subscription_that_ends_stops_for_its_thread_only() {
        let (mut app, subscriptions) = setup_tui();
        open_setup_thread(
            &mut app,
            "a",
            worktree_thread("a", true, json!([]), short_answer()),
        );
        let stages = setup_stages(["running", "pending", "pending", "pending", "pending"]);
        let item = setup_item("a", "running", 1, stages);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        // T3 ends the stream, as dropping its end does here.
        subscriptions.lock().unwrap().clear();
        assert!(!pump(&mut app));
        assert!(app.open.as_ref().unwrap().setup.events.is_none());
        // The card keeps what it had, with no clock to move.
        assert!(shows(&screen(&mut app, 80, 40), "Setting up worktree…"));
        assert!(!needs_tick(app.open.as_ref(), &app.sidebar));
        // This thread doesn't ask again. Another thread does.
        let thread = json!({"id": "a", "worktreePath": "/work/repo-a"});
        app.on_thread_event(live(2, "thread.updated", thread));
        assert!(subscribed(&subscriptions).is_empty());
        open_setup_thread(
            &mut app,
            "b",
            worktree_thread("b", true, json!([]), short_answer()),
        );
        assert_eq!(subscribed(&subscriptions), ["b"]);
        // The thread's watch closing closes its setup subscription too.
        app.on_thread_event(WatchEvent::Failed("thread closed".into()));
        assert!(setup_sender(&subscriptions, 0).is_closed());
    }

    /// Thread `id` as the shell lists it, titled `title`, with its worktree's path when
    /// `worktree`.
    fn listed_setup_thread(id: &str, title: &str, worktree: bool) -> Value {
        let mut thread = listed(id, title, "idle");
        thread["worktreePath"] = if worktree {
            json!(format!("/work/repo-{id}"))
        } else {
            Value::Null
        };
        thread
    }

    #[test]
    fn a_closed_thread_watch_opens_no_setup_subscription_again() {
        // A thread in its worktree, and a thread whose run is preparing a worktree it has no
        // path for yet. Once the watch closes, the tick doesn't count the run, and the sidebar
        // is hidden, so only the card could ask for the tick.
        for (case, worktree, runs) in [
            ("worktree", true, json!([])),
            ("preparing", false, json!([setup_run("preparing", false)])),
        ] {
            let (mut app, subscriptions) = setup_tui();
            open_setup_thread(
                &mut app,
                "t",
                worktree_thread("t", worktree, runs, short_answer()),
            );
            let listing = |title: &str| listed_setup_thread("t", title, worktree);
            app.on_shell_event(shell_snapshot(vec![listing("Fix login")]));
            assert_eq!(subscribed(&subscriptions), ["t"], "{case}");
            let stages = setup_stages(["done", "running", "pending", "pending", "pending"]);
            let running = setup_item("t", "running", 1, stages);
            assert!(
                deliver(&mut app, &subscriptions, WatchEvent::Item(running.clone())),
                "{case}"
            );
            assert!(
                shows(&screen(&mut app, 80, 40), "Setting up worktree…"),
                "{case}"
            );
            assert!(app.open.as_ref().unwrap().setup.ticks(), "{case}");

            // The thread's watch fails for good, and its setup subscription closes with it.
            app.on_thread_event(WatchEvent::Failed("thread closed".into()));
            assert!(setup_sender(&subscriptions, 0).is_closed(), "{case}");

            // The shell goes on changing the thread and lists it again after a reconnect. The
            // worktree, or the preparing run as the closed watch last saw it, is still there.
            for event in [
                thread_updated(2, listing("Fix the login page")),
                thread_updated(3, listing("Fix the login form")),
                shell_snapshot(vec![listing("Fix the login form")]),
                thread_updated(2, listing("Fix login")),
            ] {
                app.on_shell_event(event);
                assert_eq!(subscribed(&subscriptions), ["t"], "{case}");
            }
            // T3 sends the running setup down any subscription that is open.
            for (_, sender) in subscriptions.lock().unwrap().iter() {
                let _ = sender.send(WatchEvent::Item(running.clone()));
            }
            assert!(!pump(&mut app), "{case}");
            // The card keeps the setup it had and asks for no tick.
            assert!(
                shows(&screen(&mut app, 80, 40), "Setting up worktree…"),
                "{case}"
            );
            assert!(!needs_tick(app.open.as_ref(), &app.sidebar), "{case}");
            assert!(setup_sender(&subscriptions, 0).is_closed(), "{case}");
        }
    }

    #[test]
    fn a_connecting_or_reconnecting_watch_and_a_new_watch_follow_the_setup() {
        let (mut app, subscriptions) = setup_tui();
        let connection = |app: &App| app.open.as_ref().unwrap().connection.clone();
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", false, json!([]), short_answer()),
        );
        assert!(subscribed(&subscriptions).is_empty());
        // The shell lists the thread's worktree while its watch is still connecting.
        assert_eq!(connection(&app), "connecting");
        app.on_shell_event(shell_snapshot(vec![listed_setup_thread("t", "Fix", true)]));
        assert_eq!(subscribed(&subscriptions), ["t"]);
        // While the watch reconnects, the worktree going closes the subscription and the
        // worktree coming back opens another.
        let reconnecting = WatchEvent::Reconnecting {
            reason: "T3 connection lost: socket ended".into(),
            retry_in: Duration::from_millis(250),
        };
        app.on_thread_event(reconnecting);
        assert!(connection(&app).starts_with("reconnecting"));
        app.on_shell_event(thread_updated(2, listed_setup_thread("t", "Fix", false)));
        assert!(setup_sender(&subscriptions, 0).is_closed());
        app.on_shell_event(thread_updated(3, listed_setup_thread("t", "Fix", true)));
        assert_eq!(subscribed(&subscriptions), ["t", "t"]);
        assert!(!setup_sender(&subscriptions, 1).is_closed());
        // The watch closes. Another thread still subscribes, and so does this one when it
        // opens again with a new watch.
        app.on_thread_event(WatchEvent::Failed("thread closed".into()));
        assert!(setup_sender(&subscriptions, 1).is_closed());
        open_setup_thread(
            &mut app,
            "b",
            worktree_thread("b", true, json!([]), short_answer()),
        );
        assert_eq!(subscribed(&subscriptions), ["t", "t", "b"]);
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", false, json!([]), short_answer()),
        );
        assert_eq!(subscribed(&subscriptions), ["t", "t", "b", "t"]);
        assert!(!setup_sender(&subscriptions, 3).is_closed());
    }

    #[test]
    fn one_subscription_lasts_through_frames_keys_answers_and_config() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let (mut app, subscriptions) = setup_tui();
        // A thread with no worktree and no run preparing one has no setup to watch.
        let runs = json!([setup_run("completed", true)]);
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", false, runs, short_answer()),
        );
        assert!(subscribed(&subscriptions).is_empty());
        // A run preparing one opens the subscription.
        let preparing = json!({"id": "r2", "status": "preparing", "ordinal": 2, "startedAt": null});
        app.on_thread_event(live(2, "run.created", preparing));
        assert_eq!(subscribed(&subscriptions), ["t"]);
        // T3 records the worktree on the thread, and the shell lists it there too.
        let thread = json!({"id": "t", "worktreePath": "/work/repo-t"});
        app.on_thread_event(live(3, "thread.updated", thread));
        let mut listed_thread = listed("t", "Fix login", "running");
        listed_thread["worktreePath"] = json!("/work/repo-t");
        app.on_shell_event(shell_snapshot(vec![listed_thread.clone()]));
        app.on_shell_event(thread_updated(2, listed_thread));
        let stages = setup_stages(["done", "running", "pending", "pending", "pending"]);
        let item = setup_item("t", "running", 1, stages);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        // Frames, the pieces of a streamed answer, keys, the config and a toast.
        for piece in 1..=5u64 {
            screen(&mut app, 80, 30);
            let answer = json!({"id": "a9", "type": "assistant_message", "ordinal": 9,
                "text": "word ".repeat(piece as usize)});
            app.on_thread_event(live(3 + piece, "turn-item.updated", answer));
        }
        press(&mut app, KeyCode::Esc);
        for key in ['j', 'k', 'g', 'G', 'w'] {
            press(&mut app, KeyCode::Char(key));
            screen(&mut app, 80, 30);
        }
        let config = json!({"version": 1, "type": "snapshot", "config": {"providers": []}});
        app.on_config_event(WatchEvent::Item(config));
        app.on_action_result(ActionResult::Info("Copied".into()));
        screen(&mut app, 80, 30);
        assert_eq!(subscribed(&subscriptions), ["t"]);
        assert!(!setup_sender(&subscriptions, 0).is_closed());

        // A settled card is laid out once, however many frames, keys and pieces follow, until
        // the width changes.
        let stages = setup_stages(["done", "done", "skipped", "done", "done"]);
        let item = setup_item("t", "done", 2, stages);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        screen(&mut app, 80, 30);
        let layouts = |app: &App| app.open.as_ref().unwrap().setup.layouts;
        let before = layouts(&app);
        for piece in 6..=10u64 {
            let answer = json!({"id": "a9", "type": "assistant_message", "ordinal": 9,
                "text": "word ".repeat(piece as usize)});
            app.on_thread_event(live(3 + piece, "turn-item.updated", answer));
            press(&mut app, KeyCode::Char('k'));
            screen(&mut app, 80, 30);
        }
        assert_eq!(layouts(&app), before);
        screen(&mut app, 90, 30);
        assert_eq!(layouts(&app), before + 1);
        assert_eq!(subscribed(&subscriptions), ["t"]);
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn a_malformed_setup_draws_nothing_and_breaks_nothing() {
        let (mut app, subscriptions) = setup_tui();
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", true, json!([]), short_answer()),
        );
        let stages = setup_stages(["done", "running", "pending", "pending", "pending"]);
        let good = setup_item("t", "running", 1, stages);
        let broken = |key: &str, value: Value| {
            let mut item = good.clone();
            item[key] = value;
            item
        };
        for item in [
            json!("running"),
            json!([good.clone()]),
            json!({"threadId": "t"}),
            broken("threadId", json!(7)),
            broken("phase", json!("paused")),
            broken("sequence", json!(-1)),
            broken("sequence", json!(1.5)),
            broken("stages", json!({"fetch": "done"})),
            broken("startedAt", Value::Null),
        ] {
            assert!(
                !deliver(&mut app, &subscriptions, WatchEvent::Item(item.clone())),
                "{item}"
            );
        }
        assert!(!shows(&screen(&mut app, 80, 40), "worktree"));
        assert!(card_lines(&app).is_empty());
        // Stages and fields T3 doesn't send that way read as absent.
        let mut odd = good.clone();
        odd["stages"] = json!([
            {"id": "deploy", "status": "running"},
            {"id": "fetch", "status": "spinning"},
            {"id": "checkout", "status": "running", "percent": 250, "detail": 7, "tail": "x"},
            3,
            null,
        ]);
        odd["branch"] = json!(42);
        odd["setupScript"] = json!("bun install");
        odd["endedAt"] = json!("soon");
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(odd)));
        let rows = screen(&mut app, 80, 40);
        let checkout = &rows[row_showing(&rows, "Check out files") as usize];
        assert!(!checkout.contains('%'), "{checkout}");
        assert!(!shows(&rows, "deploy") && !shows(&rows, "Fetch base branch"));
        let lines = card_lines(&app);
        assert!(
            lines[0].trim_end().ends_with("Setting up worktree…"),
            "{lines:?}"
        );
        assert_eq!(subscribed(&subscriptions), ["t"]);
    }

    #[test]
    fn setup_text_reaches_the_screen_clean() {
        let (mut app, subscriptions) = setup_tui();
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", true, json!([]), short_answer()),
        );
        let mut stages = setup_stages(["done", "warning", "skipped", "failed", "pending"]);
        stages[1]["detail"] = json!("\u{1b}[31m詳細 👩\u{200d}💻 e\u{301}\u{7}");
        stages[3]["tail"] = json!(["\u{1b}]0;title\u{7}漢字 output\r", "\u{9b}2Jcleared\u{7f}"]);
        let mut item = setup_item("t", "failed", 1, stages);
        item["error"] = json!("\u{1b}]52;c;aGk=\u{7}checkout failed\u{85}\r\nfor 枝");
        item["branch"] = json!("\u{1b}[1mfeature/漢字\u{1b}[0m");
        item["setupScript"]["name"] = json!("\u{1b}[2JInstall\u{7}");
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        screen(&mut app, 80, 40);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('w'));
        let buffer = cells(&mut app, 80, 40);
        let symbols: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(!symbols.chars().any(char::is_control), "{symbols:?}");
        // CJK, a joined emoji and an accent stay whole. The wide characters are read from the
        // card, since a buffer holds a blank cell after each.
        let lines = card_lines(&app).join("\n");
        assert!(
            !lines.chars().any(|c| c.is_control() && c != '\n'),
            "{lines:?}"
        );
        for kept in [
            "詳細 👩\u{200d}💻 e\u{301}",
            "漢字 output",
            "2Jcleared",
            "checkout failed",
            "for 枝",
            "feature/漢字",
            "Install",
        ] {
            assert!(lines.contains(kept), "{kept:?} in {lines}");
        }
    }

    #[test]
    fn w_and_a_click_open_details_in_place_and_take_nothing_else() {
        let (mut app, subscriptions) = setup_tui();
        let runs = json!([setup_run("preparing", false)]);
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", true, runs, short_answer()),
        );
        let stages = setup_stages(["done", "done", "skipped", "done", "done"]);
        let item = setup_item("t", "done", 1, stages);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        let rows = screen(&mut app, 120, 40);
        let at = row_showing(&rows, "› Details");
        // Typed in the composer, `w` is text.
        assert_eq!(app.focus, Focus::Composer);
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.composer.text(), "w");
        let rows = screen(&mut app, 120, 40);
        assert_eq!(row_showing(&rows, "› Details"), at);
        // In the transcript, the status line offers it and `w` opens Details under its row,
        // which stays where it was.
        press(&mut app, KeyCode::Esc);
        let rows = screen(&mut app, 120, 40);
        assert!(rows[39].contains("w show setup details"), "{}", rows[39]);
        press(&mut app, KeyCode::Char('w'));
        // A second press before the next frame does nothing.
        press(&mut app, KeyCode::Char('w'));
        let rows = screen(&mut app, 120, 40);
        assert_eq!(row_showing(&rows, "⌄ Details"), at);
        assert!(rows[at as usize + 1].contains("Branch  t3code/t"));
        assert!(shows(&rows, "Path    /work/repo-t") && shows(&rows, "Setup   bun install"));
        assert!(rows[39].contains("w hide setup details"), "{}", rows[39]);
        // Alt+W is the error banner's key and leaves Details alone.
        alt(&mut app, 'w');
        let rows = screen(&mut app, 120, 40);
        assert_eq!(row_showing(&rows, "⌄ Details"), at);
        // A click on the row closes it, and the draft stays.
        assert!(mouse(&mut app, CLICK, 10, at));
        let rows = screen(&mut app, 120, 40);
        assert_eq!(row_showing(&rows, "› Details"), at);
        assert!(!shows(&rows, "Branch  t3code/t"));
        assert_eq!(app.composer.text(), "w");
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn a_click_needs_the_card_where_the_last_frame_drew_it() {
        let (mut app, subscriptions) = setup_tui();
        let runs = json!([setup_run("preparing", false)]);
        let answers = answers().into_iter().take(2).collect();
        open_setup_thread(&mut app, "t", worktree_thread("t", true, runs, answers));
        let ready = || setup_stages(["done", "done", "skipped", "done", "done"]);
        let item = setup_item("t", "done", 1, ready());
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        press(&mut app, KeyCode::Esc);
        let opened = |rows: &[String]| shows(rows, "⌄ Details");
        // Scrolled since the frame.
        let at = row_showing(&screen(&mut app, 80, 30), "› Details");
        press(&mut app, KeyCode::Char('k'));
        mouse(&mut app, CLICK, 10, at);
        assert!(!opened(&screen(&mut app, 80, 30)));
        // The setup changed since the frame.
        let at = row_showing(&screen(&mut app, 80, 30), "› Details");
        let mut changed = ready();
        changed[3]["detail"] = json!("Installed 812 packages");
        let item = setup_item("t", "done", 2, changed);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        mouse(&mut app, CLICK, 10, at);
        press(&mut app, KeyCode::Char('w'));
        assert!(!opened(&screen(&mut app, 80, 30)));
        // The screen resized since the frame.
        let at = row_showing(&screen(&mut app, 80, 30), "› Details");
        app.on_terminal_event(Event::Resize(100, 30));
        mouse(&mut app, CLICK, 10, at);
        press(&mut app, KeyCode::Char('w'));
        assert!(!opened(&screen(&mut app, 80, 30)));
        // The sidebar came back since the frame.
        let at = row_showing(&screen(&mut app, 80, 30), "› Details");
        ctrl_b(&mut app);
        mouse(&mut app, CLICK, 10, at);
        ctrl_b(&mut app);
        assert!(!opened(&screen(&mut app, 80, 30)));
        // With nothing moved, the click opens it.
        let at = row_showing(&screen(&mut app, 80, 30), "› Details");
        assert!(mouse(&mut app, CLICK, 10, at));
        let rows = screen(&mut app, 80, 30);
        assert_eq!(row_showing(&rows, "⌄ Details"), at);
    }

    #[test]
    fn details_open_and_close_with_their_row_in_place() {
        // Following the bottom of a thread a little taller than the screen, and at the top of
        // a long one.
        for (count, key) in [(2, KeyCode::Char('G')), (20, KeyCode::Char('g'))] {
            let (mut app, subscriptions) = setup_tui();
            let runs = json!([setup_run("preparing", false)]);
            let answers = answers().into_iter().take(count).collect();
            open_setup_thread(&mut app, "t", worktree_thread("t", true, runs, answers));
            let stages = setup_stages(["done", "done", "skipped", "done", "done"]);
            let mut item = setup_item("t", "done", 1, stages);
            // A done setup with its end, so its elapsed time can't tick over between the
            // frames compared below.
            item["endedAt"] = json!("2026-10-07T00:00:12.000Z");
            assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
            press(&mut app, KeyCode::Esc);
            press(&mut app, key);
            let rows = screen(&mut app, 80, 30);
            let at = row_showing(&rows, "› Details");
            let above = rows[2..at as usize].to_vec();
            press(&mut app, KeyCode::Char('w'));
            let rows = screen(&mut app, 80, 30);
            assert_eq!(row_showing(&rows, "⌄ Details"), at, "{key:?}");
            assert_eq!(rows[2..at as usize], above, "{key:?}");
            assert!(rows[at as usize + 1].contains("Branch"));
            press(&mut app, KeyCode::Char('w'));
            let rows = screen(&mut app, 80, 30);
            assert_eq!(row_showing(&rows, "› Details"), at, "{key:?}");
        }
    }

    #[test]
    fn a_card_growing_above_the_screen_leaves_the_rows_being_read() {
        let (mut app, subscriptions) = setup_tui();
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", true, json!([]), answers()),
        );
        let stages = setup_stages(["done", "done", "skipped", "running", "pending"]);
        let item = setup_item("t", "running", 1, stages);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        press(&mut app, KeyCode::Esc);
        for _ in 0..40 {
            press(&mut app, KeyCode::Char('k'));
        }
        let before = screen(&mut app, 80, 30);
        assert!(!shows(&before, "Setting up worktree"));
        // Out of view, a running card asks for no tick.
        assert!(!needs_tick(app.open.as_ref(), &app.sidebar));
        // The script's output starts, and the card grows by its four rows.
        let mut stages = setup_stages(["done", "done", "skipped", "running", "pending"]);
        stages[3]["tail"] = json!(["Resolving packages"]);
        let item = setup_item("t", "running", 2, stages);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        let after = screen(&mut app, 80, 30);
        assert_eq!(after[2..20], before[2..20]);
    }

    #[test]
    fn only_a_running_card_in_view_needs_the_tick() {
        let (mut app, subscriptions) = setup_tui();
        // No run, so only the card can ask for the tick.
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", true, json!([]), answers()),
        );
        let send = |app: &mut App, phase: &str, sequence: u64| {
            let stages = setup_stages(["done", "running", "pending", "pending", "pending"]);
            let item = setup_item("t", phase, sequence, stages);
            deliver(app, &subscriptions, WatchEvent::Item(item))
        };
        let ticks = |app: &App| needs_tick(app.open.as_ref(), &app.sidebar);
        assert!(send(&mut app, "running", 1));
        // Following the bottom of a long thread, the card is far above the screen.
        screen(&mut app, 80, 30);
        assert!(!ticks(&app));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('g'));
        assert!(shows(&screen(&mut app, 80, 30), "Setting up worktree…"));
        assert!(ticks(&app));
        // A setup that ended keeps its card with no clock to move.
        for (phase, sequence) in [("done", 2), ("failed", 3), ("cancelled", 4)] {
            assert!(send(&mut app, phase, sequence));
            assert!(shows(&screen(&mut app, 80, 30), "Worktree"));
            assert!(!ticks(&app), "{phase}");
        }
        // A running setup held after its subscription closed, as when the thread loses its
        // worktree, keeps its card and no clock.
        assert!(send(&mut app, "running", 5));
        screen(&mut app, 80, 30);
        assert!(ticks(&app));
        let thread = json!({"id": "t", "worktreePath": null});
        app.on_thread_event(live(2, "thread.updated", thread));
        assert!(shows(&screen(&mut app, 80, 30), "Setting up worktree…"));
        assert!(!ticks(&app));
    }

    #[test]
    fn the_card_fits_narrow_and_tiny_windows() {
        let (mut app, subscriptions) = setup_tui();
        let runs = json!([setup_run("preparing", false)]);
        open_setup_thread(
            &mut app,
            "t",
            worktree_thread("t", true, runs, short_answer()),
        );
        let mut stages = setup_stages(["done", "running", "warning", "failed", "pending"]);
        stages[1]["percent"] = json!(42);
        stages[2]["detail"] = json!("詳細な説明 ".repeat(20));
        stages[3]["tail"] = json!(["x".repeat(400), "漢字".repeat(200)]);
        let mut item = setup_item("t", "failed", 1, stages);
        item["error"] = json!("fatal: could not create work tree dir ".repeat(20));
        item["worktreePath"] = json!(format!("/work/{}", "deep/".repeat(80)));
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        screen(&mut app, 80, 40);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('w'));
        for (width, height) in [(80, 40), (40, 12), (24, 12), (16, 12), (12, 30)] {
            screen(&mut app, width, height);
            let room = (width as usize).saturating_sub(3).max(10);
            let card = app.open.as_ref().and_then(|o| o.setup.card());
            let card = card.expect("a card");
            assert!(text(&card.lines).iter().any(|row| row.contains("Details")));
            for line in &card.lines {
                assert!(line.width() <= room, "{width}: {line:?}");
            }
        }
    }

    #[test]
    fn with_no_prompt_the_card_follows_the_last_block() {
        let (mut app, subscriptions) = setup_tui();
        let projection = json!({"thread": {"id": "t", "worktreePath": "/work/repo-t"},
            "turnItems": short_answer(), "runs": []});
        let snapshot = json!({"kind": "snapshot", "snapshotSequence": 1, "projection": projection});
        open_setup_thread(&mut app, "t", WatchEvent::Item(snapshot));
        let stages = setup_stages(["running", "pending", "pending", "pending", "pending"]);
        let item = setup_item("t", "running", 1, stages);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(item)));
        let rows = screen(&mut app, 80, 40);
        let answer = row_showing(&rows, "Looking at the login page");
        assert!(answer < row_showing(&rows, "Setting up worktree…"));
        // Once a prompt arrives, the card moves under it.
        let prompt = json!({"id": "u0", "type": "user_message", "ordinal": 2, "runId": "r1",
            "text": "Fix the login page"});
        app.on_thread_event(live(2, "turn-item.updated", prompt));
        let rows = screen(&mut app, 80, 40);
        let prompt = row_showing(&rows, "Fix the login page");
        assert!(row_showing(&rows, "Looking at the login page") < prompt);
        assert!(prompt < row_showing(&rows, "Setting up worktree…"));
    }

    // ---- the environment and the sidebar's feed ----

    /// The TUI as `tui` starts it with no settings file, connected to `origin`, which
    /// discovery read with the descriptor's `label`.
    fn tui_at(label: Option<&str>, origin: &str) -> App {
        let mut client = Client::offline();
        client.runtime.label = label.map(str::to_string);
        client.runtime.origin = origin.to_string();
        let mut app = App::new(
            Arc::new(client),
            Theme::new(Depth::TrueColor),
            &Settings::default(),
            mpsc::unbounded_channel().0,
        );
        app.watch_setup = Box::new(|_: &str| mpsc::unbounded_channel().1);
        app
    }

    /// T3's marker that the sidebar's feed has caught up.
    fn caught_up() -> WatchEvent {
        WatchEvent::Item(json!({"kind": "synchronized"}))
    }

    /// The sidebar's feed dropping, as its watcher says before it subscribes again.
    fn shell_dropped() -> WatchEvent {
        WatchEvent::Reconnecting {
            reason: "T3 connection lost: socket ended".into(),
            retry_in: Duration::from_millis(250),
        }
    }

    /// What one frame draws around the conversation.
    #[derive(Debug)]
    struct Chrome {
        /// The sidebar's first row under the inset, with the wordmark and the dot.
        wordmark: String,
        /// The sidebar's row under it.
        environment: String,
        /// The sidebar's list.
        list: String,
        /// The main column's title row, with the toggle and the breadcrumb, up to the details'
        /// button.
        header: String,
        /// The main column's bottom row.
        status: String,
        /// The colors of the dots in the sidebar's wordmark row and the main column's title
        /// row.
        sidebar_dot: Option<Color>,
        header_dot: Option<Color>,
    }

    /// Draws one frame `width` columns by `height` rows and reads its chrome.
    fn chrome(app: &mut App, width: u16, height: u16) -> Chrome {
        let buffer = cells(app, width, height);
        // The main column starts after the sidebar's edge, at 0 with the sidebar hidden.
        let main = app.sidebar.border.right();
        let end = match app.details.button {
            button if button.width > 0 => button.x,
            _ => width,
        };
        // A screen with room has a blank row over the wordmark and the title row.
        let top = top_inset(height);
        let dot = |columns: std::ops::Range<u16>| {
            columns
                .map(|x| &buffer[(x, top)])
                .find(|cell| cell.symbol() == "●")
                .map(|cell| cell.fg)
        };
        Chrome {
            wordmark: text_in(&buffer, Rect::new(0, top, main, 1)),
            environment: text_in(&buffer, Rect::new(0, top + 1, main, 1)),
            list: text_in(&buffer, app.sidebar.list),
            header: text_in(&buffer, Rect::new(main, top, end - main, 1)),
            status: text_in(&buffer, Rect::new(main, height - 1, width - main, 1)),
            sidebar_dot: dot(0..main),
            header_dot: dot(main..width),
        }
    }

    #[test]
    fn the_sidebar_dot_follows_its_feed_and_not_the_open_thread() {
        let mut app = tui_at(Some("Studio Mac"), "http://127.0.0.1:3773");
        let (emerald, warning) = (app.theme.emerald, app.theme.warning);
        // Until T3 says the feed caught up, the dot is amber and says why, after the
        // snapshot too. With no thread open, the status line names the feed.
        let listing = shell_snapshot(vec![listed("t", "Fix the login page", "idle")]);
        for event in [None, Some(listing)] {
            if let Some(event) = event {
                app.on_shell_event(event);
            }
            let drawn = chrome(&mut app, 80, 24);
            assert!(drawn.wordmark.contains("connecting ●"), "{drawn:?}");
            assert_eq!(drawn.sidebar_dot, Some(warning));
            assert!(drawn.status.contains("● connecting  ·"), "{drawn:?}");
        }
        app.on_shell_event(caught_up());
        let drawn = chrome(&mut app, 80, 24);
        assert!(!drawn.wordmark.contains("connecting"), "{drawn:?}");
        assert_eq!(drawn.sidebar_dot, Some(emerald));
        assert!(drawn.status.contains("● live  ·"), "{drawn:?}");
        assert_eq!(drawn.environment.trim(), "Studio Mac");

        // The open thread's watch closes. The status line and the breadcrumb say so, and the
        // sidebar's dot stays green for the feed that is still live.
        app.open_thread(long_thread());
        let drawn = chrome(&mut app, 80, 24);
        assert!(!drawn.header.contains("closed"), "{drawn:?}");
        app.on_thread_event(WatchEvent::Failed("thread closed".into()));
        press(&mut app, KeyCode::End);
        let drawn = chrome(&mut app, 80, 24);
        assert!(drawn.status.contains("● closed  ·"), "{drawn:?}");
        assert!(drawn.header.contains("Fix the login"), "{drawn:?}");
        assert!(drawn.header.trim_end().ends_with("closed"), "{drawn:?}");
        assert_eq!(drawn.header_dot, None, "the sidebar shows the environment");
        assert!(!drawn.wordmark.contains("closed"), "{drawn:?}");
        assert_eq!(drawn.sidebar_dot, Some(emerald));
        assert_eq!(drawn.environment.trim(), "Studio Mac");
    }

    #[test]
    fn a_dropped_or_closed_feed_shows_beside_a_live_thread() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = tui_at(Some("Studio Mac"), "http://127.0.0.1:3773");
        let (emerald, warning, error) = (app.theme.emerald, app.theme.warning, app.theme.error);
        let listing = || shell_snapshot(vec![listed("t", "Fix the login page", "idle")]);
        app.on_shell_event(listing());
        app.on_shell_event(caught_up());
        app.open_thread(long_thread());
        let started = reads(&runtime);

        // The feed drops while the thread's watch stays live. The sidebar says so and keeps
        // its rows and the environment. The status line and the breadcrumb are the thread's.
        app.on_shell_event(shell_dropped());
        let drawn = chrome(&mut app, 80, 24);
        assert!(drawn.wordmark.contains("reconnecting ●"), "{drawn:?}");
        assert_eq!(drawn.sidebar_dot, Some(warning));
        assert!(drawn.list.contains("Fix the"), "{drawn:?}");
        assert!(drawn.environment.contains("Studio Mac"), "{drawn:?}");
        assert!(drawn.status.contains("● live  ·"), "{drawn:?}");
        assert!(!drawn.header.contains("connecting"), "{drawn:?}");
        // A snapshot after the reconnect isn't enough. The dot turns green when T3 says the
        // feed caught up.
        app.on_shell_event(listing());
        let drawn = chrome(&mut app, 80, 24);
        assert!(drawn.wordmark.contains("reconnecting ●"), "{drawn:?}");
        app.on_shell_event(caught_up());
        let drawn = chrome(&mut app, 80, 24);
        assert!(!drawn.wordmark.contains("connecting"), "{drawn:?}");
        assert_eq!(drawn.sidebar_dot, Some(emerald));

        // T3 refuses the feed for good. The toast says why. Once a key takes it, the sidebar
        // still says the feed closed, beside the thread that is still live.
        let refused = "unknown method: orchestration.subscribeShell";
        app.on_shell_event(WatchEvent::Failed(refused.into()));
        let drawn = chrome(&mut app, 80, 24);
        assert!(drawn.status.contains(refused), "{drawn:?}");
        assert!(drawn.wordmark.contains("closed ●"), "{drawn:?}");
        press(&mut app, KeyCode::End);
        let drawn = chrome(&mut app, 80, 24);
        assert!(drawn.status.contains("● live  ·"), "{drawn:?}");
        assert!(drawn.wordmark.contains("closed ●"), "{drawn:?}");
        assert_eq!(drawn.sidebar_dot, Some(error));
        assert!(drawn.list.contains("Fix the"), "{drawn:?}");
        assert!(drawn.environment.contains("Studio Mac"), "{drawn:?}");

        // With the sidebar hidden, the breadcrumb's row shows the feed's state instead.
        ctrl_b(&mut app);
        let drawn = chrome(&mut app, 120, 24);
        assert_eq!(drawn.header_dot, Some(error));
        assert!(drawn.header.contains("● closed · Studio Mac"), "{drawn:?}");
        assert!(drawn.header.contains("Fix the login page"), "{drawn:?}");
        assert!(drawn.status.contains("● live  ·"), "{drawn:?}");
        // None of it asked T3 for anything or needs the tick.
        assert_eq!(reads(&runtime), started);
        assert!(!needs_tick(app.open.as_ref(), &app.sidebar));
    }

    #[test]
    fn with_no_thread_open_the_status_line_names_the_feed() {
        let mut app = tui_at(Some("Studio Mac"), "http://127.0.0.1:3773");
        app.on_shell_event(caught_up());
        app.on_shell_event(shell_dropped());
        let drawn = chrome(&mut app, 80, 24);
        assert!(drawn.status.contains("● reconnecting  ·"), "{drawn:?}");
        app.on_shell_event(WatchEvent::Failed("T3 refused the subscription".into()));
        // The feed's watcher has stopped, so the state stays closed through later keys and
        // frames.
        for _ in 0..2 {
            press(&mut app, KeyCode::End);
            let drawn = chrome(&mut app, 80, 24);
            assert!(drawn.status.contains("● closed  ·"), "{drawn:?}");
            assert!(drawn.wordmark.contains("closed ●"), "{drawn:?}");
            assert_eq!(drawn.sidebar_dot, Some(app.theme.error));
        }
    }

    #[test]
    fn the_environment_shows_without_the_sidebar_and_never_its_credentials() {
        let origin = "http://alice:hunter2@127.0.0.1:3773/private-path?token=sekrit#frag";
        let mut app = tui_at(Some("Studio Mac"), origin);
        app.on_shell_event(caught_up());
        // With the sidebar, it is under the wordmark, and the breadcrumb's row doesn't
        // repeat it.
        let drawn = chrome(&mut app, 120, 24);
        assert_eq!(drawn.environment.trim(), "Studio Mac · 127.0.0.1:3773");
        assert_eq!(drawn.header_dot, None);
        assert!(!drawn.header.contains("Studio"), "{drawn:?}");
        let drawn = chrome(&mut app, 40, 24);
        assert_eq!(drawn.environment.trim(), "Studio Mac");

        // Hidden, the breadcrumb's row ends with the dot and as much of the environment as a
        // third of the row holds, after the first 16 columns of the title.
        ctrl_b(&mut app);
        let drawn = chrome(&mut app, 120, 24);
        let header = drawn.header.trim_end();
        assert!(
            header.ends_with("  ● Studio Mac · 127.0.0.1:3773"),
            "{header:?}"
        );
        assert!(
            header.contains("Pick a thread in the sidebar"),
            "{header:?}"
        );
        assert_eq!(drawn.header_dot, Some(app.theme.emerald));
        let header = chrome(&mut app, 80, 24).header;
        assert!(header.trim_end().ends_with("  ● Studio Mac"), "{header:?}");
        assert!(
            header.contains("Pick a thread in the sidebar"),
            "{header:?}"
        );
        // The details' button ends the padded row. ◨ is one column wide, so " ◨ " takes three
        // columns, with a blank one before it. At 40 the row's 38 columns hold the toggle and
        // its gap, four, then the breadcrumb, 30, then the gap and the button, four. Without
        // the button the breadcrumb had 34.
        let header = chrome(&mut app, 40, 24).header;
        assert_eq!(app.details.button, Rect::new(36, 1, 3, 1));
        assert_eq!(header, "  ◧  Pick a thread in th…  ● Studi… ");
        // Too narrow for it beside the title's 16 columns, the row is the title's.
        let drawn = chrome(&mut app, 24, 24);
        assert_eq!(drawn.header_dot, None);
        assert_eq!(drawn.header, "  ◧  Pick a thread… ");

        // At no width, hidden or shown, does any of the origin but its host and port show.
        for hidden in [true, false] {
            for width in [24, 40, 80, 120, 200] {
                let rows = screen(&mut app, width, 24).join("\n");
                for secret in [
                    "alice",
                    "hunter2",
                    "private-path",
                    "token",
                    "sekrit",
                    "frag",
                ] {
                    assert!(
                        !rows.contains(secret),
                        "{hidden} {width}: {secret} in {rows}"
                    );
                }
            }
            if hidden {
                ctrl_b(&mut app);
            }
        }
        // Shown again, it is back under the wordmark.
        let drawn = chrome(&mut app, 120, 24);
        assert_eq!(drawn.environment.trim(), "Studio Mac · 127.0.0.1:3773");
        assert_eq!(drawn.header_dot, None);
    }

    #[test]
    fn without_a_label_the_address_stands_in_and_without_either_the_dot_is_alone() {
        let mut app = tui_at(None, "http://[::1]:3773");
        app.on_shell_event(caught_up());
        assert_eq!(chrome(&mut app, 80, 24).environment.trim(), "[::1]:3773");
        ctrl_b(&mut app);
        let header = chrome(&mut app, 80, 24).header;
        assert!(header.trim_end().ends_with("  ● [::1]:3773"), "{header:?}");

        let mut app = tui_at(Some(" \u{1b}\u{7} "), "not a url");
        app.on_shell_event(caught_up());
        assert_eq!(chrome(&mut app, 80, 24).environment.trim(), "");
        ctrl_b(&mut app);
        let drawn = chrome(&mut app, 80, 24);
        let header = drawn
            .header
            .trim_end()
            .strip_suffix("  ●")
            .expect("the dot at the end");
        assert!(header.trim_end().ends_with("in the sidebar"), "{header:?}");
        assert_eq!(drawn.header_dot, Some(app.theme.emerald));
    }

    /// The text after the last dot in the breadcrumb's row, where `environment::chip` puts the
    /// environment while the sidebar doesn't show it, or nothing when the row has no dot.
    fn header_identity(drawn: &Chrome) -> &str {
        drawn
            .header
            .trim_end()
            .rsplit_once('●')
            .map_or("", |(_, identity)| identity.trim())
    }

    #[test]
    fn the_sidebar_and_the_header_keep_the_whole_port() {
        let whole = "Studio Mac · 127.0.0.1:51024";
        let mut app = tui_at(Some("Studio Mac"), "http://127.0.0.1:51024");
        app.on_shell_event(caught_up());
        app.focus = Focus::Composer;
        app.composer.insert_str("half a thought");
        // At 80 columns the row under the wordmark has ten columns after the label, too few for
        // the address with its five-digit port, so the label shows alone.
        let drawn = chrome(&mut app, 80, 24);
        assert_eq!(drawn.environment.trim(), "Studio Mac");
        assert_eq!(drawn.header_dot, None);
        // Dragged wider, the row keeps the label alone until the whole address fits.
        let edge = app.sidebar.border.x;
        mouse(&mut app, CLICK, edge, 5);
        assert!(app.drag.is_some());
        for to in edge..edge + 8 {
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), to, 5);
            let drawn = chrome(&mut app, 80, 24);
            assert_eq!(app.sidebar.border.x, to);
            let row = usize::from(app.sidebar.list.width);
            let expected = if whole.width() <= row {
                whole
            } else {
                "Studio Mac"
            };
            assert_eq!(drawn.environment.trim(), expected, "a row of {row}");
        }
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), edge + 7, 5);
        assert_eq!(chrome(&mut app, 80, 24).environment.trim(), whole);
        assert_eq!(app.composer.text(), "half a thought");

        // Hidden, the breadcrumb's row shows the whole address or none of it, at every width.
        ctrl_b(&mut app);
        for width in 24..=200 {
            let drawn = chrome(&mut app, width, 24);
            let shown = header_identity(&drawn);
            assert!(
                shown == whole || !shown.contains(|c: char| c == '·' || c.is_ascii_digit()),
                "{width}: {drawn:?}"
            );
        }
        assert_eq!(header_identity(&chrome(&mut app, 120, 24)), whole);
        assert_eq!(app.composer.text(), "half a thought");
    }

    #[test]
    fn without_a_label_an_ip_address_keeps_its_whole_port() {
        let whole = "[::1]:51024";
        let mut app = tui_at(None, "http://[::1]:51024");
        app.on_shell_event(caught_up());
        assert_eq!(chrome(&mut app, 80, 24).environment.trim(), whole);
        // Shown or hidden, at every width, the sidebar's row and the breadcrumb's show the
        // whole address or none of it, never some of the port alone.
        for hidden in [false, true] {
            if hidden {
                ctrl_b(&mut app);
            }
            for width in 20..=200 {
                let drawn = chrome(&mut app, width, 24);
                for shown in [drawn.environment.trim(), header_identity(&drawn)] {
                    assert!(
                        shown.is_empty() || shown == whole,
                        "{hidden} {width}: {drawn:?}"
                    );
                }
            }
        }
        assert_eq!(header_identity(&chrome(&mut app, 120, 24)), whole);
    }

    #[test]
    fn a_hostile_label_is_cleaned_once_and_fits_every_width() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let label = format!(
            "\u{1b}]0;pwned\u{7}\u{1b}[2J名前 {}\r\nevil",
            "👩\u{200d}💻e\u{301}".repeat(5_000)
        );
        let mut app = tui_at(Some(&label), "http://[::1]:3773");
        let prepared = app.environment.clone();
        app.on_shell_event(shell_snapshot(vec![listed(
            "t",
            "Fix the login page",
            "idle",
        )]));
        app.on_shell_event(caught_up());
        app.open_thread(long_thread());
        for hidden in [false, true] {
            if hidden {
                ctrl_b(&mut app);
            }
            for width in [24, 40, 80, 120, 200] {
                let buffer = cells(&mut app, width, 24);
                for (x, y) in (0..24).flat_map(|y| (0..width).map(move |x| (x, y))) {
                    let symbol = buffer[(x, y)].symbol();
                    assert!(
                        !symbol.contains(char::is_control),
                        "{width} ({x}, {y}) {symbol:?}"
                    );
                }
                let top = text_in(&buffer, Rect::new(0, 0, width, 3));
                // The label is cut long before its end.
                assert!(!top.contains("evil"), "{top}");
                // The title keeps its columns.
                assert!(
                    width < 80 || top.contains("Fix the login page"),
                    "{width}: {top}"
                );
            }
        }
        // Answers, shell changes, an environment T3's config renames and more frames leave the
        // identity as it was made.
        let answer = json!({"id": "a20", "type": "assistant_message", "ordinal": 20,
            "text": "Done"});
        app.on_thread_event(live(2, "turn-item.updated", answer));
        app.on_shell_event(thread_updated(2, listed("t", "Fix the login form", "idle")));
        let environment = json!({"label": "Renamed", "capabilities": {}});
        let config = json!({"providers": [], "settings": {}, "environment": environment});
        app.on_config_event(WatchEvent::Item(
            json!({"version": 1, "type": "snapshot", "config": config}),
        ));
        cells(&mut app, 120, 30);
        assert_eq!(app.environment, prepared);
    }

    #[test]
    fn the_environment_takes_no_clicks_and_moves_nothing() {
        let mut app = tui_at(Some("Studio Mac"), "http://127.0.0.1:3773");
        let threads = vec![listed("a", "Alpha", "idle"), listed("b", "Bravo", "idle")];
        app.on_shell_event(shell_snapshot(threads));
        app.on_shell_event(caught_up());
        app.focus = Focus::Composer;
        app.composer.insert_str("half a thought");
        chrome(&mut app, 80, 24);
        // The list starts under the blank row below the environment's.
        assert_eq!(app.sidebar.list.y, 4);
        let (border, list) = (app.sidebar.border, app.sidebar.list);
        // A click on the dot, the environment or a blank row around them does what a click on
        // the wordmark did.
        for (x, y) in [(border.x - 2, 1), (5, 2), (5, 0), (5, 3)] {
            mouse(&mut app, CLICK, x, y);
            assert_eq!(app.focus, Focus::Composer);
            assert!(app.open.is_none() && app.picker.is_none() && app.drag.is_none());
            chrome(&mut app, 80, 24);
            assert_eq!((app.sidebar.border, app.sidebar.list), (border, list));
        }
        // The edge still drags.
        mouse(&mut app, CLICK, border.x, 5);
        assert!(app.drag.is_some());
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), border.x, 5);
        assert!(app.drag.is_none());

        // Hidden, a click on the environment at the end of the breadcrumb's row leaves the
        // sidebar hidden.
        ctrl_b(&mut app);
        let header = chrome(&mut app, 120, 24).header;
        let at = header.find("Studio").expect("the environment");
        let x = header[..at].width() as u16;
        mouse(&mut app, CLICK, x, 1);
        assert!(app.sidebar_hidden);
        assert!(crate::settings::saved_in_test().sidebar_hidden);
        assert!(app.open.is_none() && app.picker.is_none() && app.drag.is_none());
        assert_eq!(app.composer.text(), "half a thought");
    }

    // ---- thread details ----

    /// The shell's snapshot with project `p`, Demo in /repo/demo, and project `q`, Other in
    /// /repo/other, listing `threads`.
    fn details_shell(threads: Vec<Value>) -> WatchEvent {
        let projects = json!([
            {"id": "p", "title": "Demo", "workspaceRoot": "/repo/demo"},
            {"id": "q", "title": "Other", "workspaceRoot": "/repo/other"},
        ]);
        let snapshot = json!({"snapshotSequence": 1, "projects": projects, "threads": threads});
        WatchEvent::Item(json!({"kind": "snapshot", "snapshot": snapshot}))
    }

    /// Thread `id` as T3 records it, in `project`, with `worktree` and `branch`.
    fn bound(id: &str, project: &str, worktree: Value, branch: Value) -> Value {
        let mut thread = listed(id, &format!("Thread {id}"), "idle");
        thread["projectId"] = json!(project);
        thread["worktreePath"] = worktree;
        thread["branch"] = branch;
        thread
    }

    /// `thread`, open and live, its projection holding it and twenty answers.
    fn open_bound(thread: Value) -> OpenThread {
        let id = str_of(&thread, "id").to_string();
        let snapshot = json!({"kind": "snapshot", "snapshotSequence": 1,
            "projection": {"thread": thread, "turnItems": answers()}});
        let mut open = OpenThread::new(id, mpsc::unbounded_channel().1);
        assert_eq!(open.apply(&snapshot), Applied::Snapshot);
        open.connection = "live".into();
        open
    }

    /// The thread `t` in a worktree of Demo on `feature/a`, as the shell lists it and its
    /// projection has it.
    fn feature_thread() -> Value {
        bound(
            "t",
            "p",
            json!("/repo/.t3/worktrees/feature-a"),
            json!("feature/a"),
        )
    }

    /// The TUI with the sidebar listing thread `t` in a worktree of Demo and thread `u` in
    /// Other's local checkout, `t` open, and a draft in the composer with its cursor before
    /// the last letter.
    fn details_tui() -> App {
        let mut app = tui(&Settings::default());
        let other = bound("u", "q", Value::Null, Value::Null);
        app.on_shell_event(details_shell(vec![feature_thread(), other]));
        app.on_shell_event(caught_up());
        app.open_thread(open_bound(feature_thread()));
        app.composer.insert_str("draft");
        press(&mut app, KeyCode::Left);
        app
    }

    /// The text of the details in `buffer`, where the last frame drew them, or none.
    fn details_text(app: &App, buffer: &ratatui::buffer::Buffer) -> String {
        app.details
            .drawn
            .map(|drawn| text_in(buffer, drawn.area))
            .unwrap_or_default()
    }

    /// Draws a frame `width` columns by `height` rows and returns the text of the details.
    fn details_after(app: &mut App, width: u16, height: u16) -> String {
        let buffer = cells(app, width, height);
        details_text(app, &buffer)
    }

    #[test]
    fn thread_details_start_closed_and_open_beside_the_conversation() {
        let mut app = details_tui();
        let buffer = cells(&mut app, 200, 50);
        assert!(!app.details.open);
        assert_eq!(app.details.drawn, None);
        let wide = app.composer_area;
        // The button ends the title row, a column in from the screen's edge, under the blank
        // row at the top.
        assert_eq!(app.details.button, Rect::new(196, 1, 3, 1));
        assert_eq!(text_in(&buffer, app.details.button), " ◨ ");

        alt(&mut app, 'b');
        assert!(app.details.open);
        assert_eq!(
            app.focus,
            Focus::Composer,
            "a pane leaves the keyboard alone"
        );
        assert!(app.layout_changed, "the next frame waits for nothing");
        let text = details_after(&mut app, 200, 50);
        let drawn = app.details.drawn.expect("the pane");
        assert_eq!(drawn.layout, details::Layout::Pane);
        // The conversation keeps all but the pane's 36 columns and a blank one.
        assert_eq!(app.composer_area.width, wide.width - 37);
        assert_eq!(drawn.area.x, app.composer_area.right() + 1);
        assert_eq!(drawn.area.y, app.transcript_area.y);
        assert_eq!(drawn.area.bottom(), app.composer_area.bottom());
        for shown in [
            "Thread details",
            "×",
            "Project",
            "Demo",
            "Workspace",
            "Worktree · feature-a",
            "/repo/.t3/worktrees/feature-a",
            "Recorded branch",
            "feature/a",
            "Environment",
            "127.0.0.1:9",
        ] {
            assert!(text.contains(shown), "{shown:?} in {text}");
        }
        // Typing still goes to the composer, at its cursor.
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.composer.text(), "drafxt");

        // Alt+B closes it, and the conversation takes its columns back.
        alt(&mut app, 'b');
        assert!(!app.details.open);
        cells(&mut app, 200, 50);
        assert_eq!(app.details.drawn, None);
        assert_eq!(app.composer_area, wide);
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.composer.text(), "drafxyt");
    }

    #[test]
    fn a_narrow_screen_shows_a_dialog_that_esc_closes_before_it_moves_focus() {
        for (width, height) in [(120, 36), (80, 24), (40, 12)] {
            let mut app = details_tui();
            app.focus = Focus::Transcript;
            cells(&mut app, width, height);
            alt(&mut app, 'b');
            assert_eq!(app.focus, Focus::Details, "{width}x{height}");
            let buffer = cells(&mut app, width, height);
            let drawn = app.details.drawn.expect("the dialog");
            assert_eq!(drawn.layout, details::Layout::Dialog);
            assert_eq!(drawn.area.intersection(app.screen), drawn.area);
            let text = details_text(&app, &buffer);
            assert!(text.contains("Thread details"), "{width}x{height}: {text}");
            let status = text_in(&buffer, Rect::new(0, height - 1, width, 1));
            assert!(
                width < 80 || status.contains("↑↓/PgUp scroll · Esc close"),
                "{status}"
            );
            // Every key but the global ones stays in the dialog. No letter reaches the composer,
            // Tab moves nothing and `q` doesn't quit.
            for code in [
                KeyCode::Char('x'),
                KeyCode::Tab,
                KeyCode::BackTab,
                KeyCode::Enter,
                KeyCode::Char('q'),
                KeyCode::Left,
            ] {
                press(&mut app, code);
            }
            assert_eq!(app.focus, Focus::Details);
            assert_eq!(app.composer.text(), "draft");
            assert!(!app.quit);
            // Esc closes it and gives the keyboard back to the transcript, which a second Esc
            // would have left for the sidebar.
            press(&mut app, KeyCode::Esc);
            assert!(!app.details.open);
            assert_eq!(app.focus, Focus::Transcript, "{width}x{height}");
            cells(&mut app, width, height);
            assert_eq!(app.details.drawn, None);
            // The draft kept its cursor.
            app.focus = Focus::Composer;
            press(&mut app, KeyCode::Char('x'));
            assert_eq!(app.composer.text(), "drafxt");
        }
    }

    #[test]
    fn the_details_follow_the_screen_and_a_pane_keeps_no_keyboard_a_dialog_took() {
        let mut app = details_tui();
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        cells(&mut app, 200, 50);
        assert_eq!(app.details.shown(), Some(details::Layout::Pane));
        assert_eq!(app.focus, Focus::Composer);
        // Narrower, the pane turns into a dialog with the keyboard.
        app.on_terminal_event(Event::Resize(120, 36));
        cells(&mut app, 120, 36);
        assert_eq!(app.details.shown(), Some(details::Layout::Dialog));
        assert_eq!(app.focus, Focus::Details);
        // Hiding the sidebar makes room for the pane again, and the keyboard goes back.
        ctrl_b(&mut app);
        cells(&mut app, 120, 36);
        assert_eq!(app.details.shown(), Some(details::Layout::Pane));
        assert_eq!(app.focus, Focus::Composer);
        // A short screen has no room for the pane either.
        cells(&mut app, 120, 14);
        assert_eq!(app.details.shown(), Some(details::Layout::Dialog));
        assert_eq!(app.focus, Focus::Details);

        // Tiny screens, then a big one again. The details fit whatever there is, and the
        // keyboard and the draft come back as they were.
        ctrl_b(&mut app);
        for (width, height) in [(0, 0), (1, 1), (2, 2), (10, 5), (19, 3), (26, 4), (200, 50)] {
            app.on_terminal_event(Event::Resize(width, height));
            for _ in 0..2 {
                cells(&mut app, width, height);
                let whole = Rect::new(0, 0, width, height);
                let drawn = app.details.drawn.expect("drawn");
                for area in [drawn.area, drawn.close, app.details.button] {
                    assert_eq!(area.intersection(whole), area, "{width}x{height}");
                }
                assert!(app.details.open);
            }
        }
        assert_eq!(app.details.shown(), Some(details::Layout::Pane));
        assert_eq!(app.focus, Focus::Composer);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.composer.text(), "drafxt");
    }

    #[test]
    fn tab_and_a_click_give_the_pane_the_keyboard_and_esc_gives_it_back() {
        let mut app = details_tui();
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        cells(&mut app, 200, 50);
        // Tab goes round the sidebar, the composer, the transcript and the pane.
        let order = focus_after(&mut app, &[KeyCode::Tab; 4]);
        assert_eq!(
            order,
            [
                Focus::Transcript,
                Focus::Details,
                Focus::Sidebar,
                Focus::Composer
            ]
        );
        let order = focus_after(&mut app, &[KeyCode::BackTab; 2]);
        assert_eq!(order, [Focus::Sidebar, Focus::Details]);
        // In the pane, letters and Enter do nothing, and the page keys scroll the pane, not
        // the transcript.
        let scrolled = app.scroll.rows();
        for code in [
            KeyCode::Char('x'),
            KeyCode::Enter,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ] {
            press(&mut app, code);
        }
        assert_eq!(app.scroll.rows(), scrolled);
        assert_eq!(app.composer.text(), "draft");
        let buffer = cells(&mut app, 200, 50);
        let status = text_in(&buffer, Rect::new(0, 49, 200, 1));
        assert!(status.contains("Esc leave · Alt+B close"), "{status}");
        // Esc gives the keyboard back to the pane before the details, and the pane stays.
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(app.details.open);

        // A click on the pane gives it the keyboard, and Alt+B gives it back as it closes.
        app.focus = Focus::Transcript;
        cells(&mut app, 200, 50);
        let drawn = app.details.drawn.expect("the pane");
        assert!(mouse(&mut app, CLICK, drawn.area.x + 3, drawn.area.y + 3));
        assert_eq!(app.focus, Focus::Details);
        assert!(app.details.open);
        alt(&mut app, 'b');
        assert!(!app.details.open);
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn the_button_and_the_pane_take_clicks_only_where_the_last_frame_drew_them() {
        let mut app = details_tui();
        cells(&mut app, 200, 50);
        let button = app.details.button;
        assert!(mouse(&mut app, CLICK, button.x + 1, button.y));
        assert!(app.details.open);
        assert_eq!(app.focus, Focus::Composer);
        // Until a frame draws the pane, another click on the button does nothing.
        assert!(!mouse(&mut app, CLICK, button.x + 1, button.y));
        assert!(app.details.open);
        cells(&mut app, 200, 50);
        let drawn = app.details.drawn.expect("the pane");
        // A click on × closes it, and one on the button opens it again.
        assert!(mouse(&mut app, CLICK, drawn.close.x + 1, drawn.close.y));
        assert!(!app.details.open);
        cells(&mut app, 200, 50);
        assert!(mouse(&mut app, CLICK, button.x, button.y));
        cells(&mut app, 200, 50);
        let drawn = app.details.drawn.expect("the pane");
        let (x, y) = (drawn.area.x + 3, drawn.area.y + 3);

        // After a change that can move what the pane shows, a resize or another thread, a
        // click on the pane or the button goes nowhere until the next frame.
        let changes: [fn(&mut App); 3] = [
            |app| {
                let moved = bound("t", "p", json!("/repo/feature-b"), json!("feature/b"));
                app.on_shell_event(thread_updated(2, moved));
            },
            |app| {
                app.on_terminal_event(Event::Resize(200, 50));
            },
            |app| app.open_thread(OpenThread::new("u".into(), mpsc::unbounded_channel().1)),
        ];
        for change in changes {
            change(&mut app);
            assert!(!mouse(&mut app, CLICK, x, y));
            assert!(!mouse(&mut app, CLICK, button.x + 1, button.y));
            assert!(app.details.open);
            assert_ne!(app.focus, Focus::Details);
            cells(&mut app, 200, 50);
        }
        // Drawn again, the pane takes the click.
        assert!(mouse(&mut app, CLICK, x, y));
        assert_eq!(app.focus, Focus::Details);
    }

    #[test]
    fn a_dialog_takes_the_mouse_until_a_click_outside_or_on_its_close_button() {
        let mut app = details_tui();
        app.focus = Focus::Transcript;
        cells(&mut app, 80, 24);
        alt(&mut app, 'b');
        // Before a frame draws it, the dialog takes clicks and does nothing with them.
        assert!(!mouse(&mut app, CLICK, 2, 3));
        assert!(app.details.open);
        cells(&mut app, 80, 24);
        let drawn = app.details.drawn.expect("the dialog");
        // A click inside it does nothing. One outside closes it and reaches nothing under it.
        assert!(!mouse(&mut app, CLICK, drawn.area.x + 3, drawn.area.y + 3));
        assert!(app.details.open);
        let selected = app.sidebar.selected_thread_id().map(str::to_string);
        assert!(mouse(&mut app, CLICK, 2, 3));
        assert!(!app.details.open);
        assert_eq!(app.focus, Focus::Transcript);
        assert_eq!(
            app.sidebar.selected_thread_id().map(str::to_string),
            selected
        );
        assert_eq!(app.open.as_ref().map(|open| open.id.as_str()), Some("t"));

        // Opened with the button, it takes the keyboard, and × closes it.
        cells(&mut app, 80, 24);
        let button = app.details.button;
        assert!(mouse(&mut app, CLICK, button.x + 1, button.y));
        assert_eq!(app.focus, Focus::Details);
        cells(&mut app, 80, 24);
        let drawn = app.details.drawn.expect("the dialog");
        assert!(mouse(&mut app, CLICK, drawn.close.x + 1, drawn.close.y));
        assert!(!app.details.open);
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn the_details_scroll_with_keys_and_the_wheel_and_stop_at_their_ends() {
        let mut app = tui(&Settings::default());
        let path = format!("/repo/{}/deep", "x".repeat(200));
        let thread = bound("t", "p", json!(path), json!("feature/a"));
        app.on_shell_event(details_shell(vec![thread.clone()]));
        app.open_thread(open_bound(thread));
        cells(&mut app, 40, 12);
        alt(&mut app, 'b');
        let top = details_after(&mut app, 40, 12);
        assert!(top.contains("Project") && top.contains("more ↓"), "{top}");
        assert!(!top.contains("Recorded branch"), "{top}");
        press(&mut app, KeyCode::Down);
        let down = details_after(&mut app, 40, 12);
        assert!(
            !down.contains("Project") && down.contains("more ↑↓"),
            "{down}"
        );
        assert!(
            down.contains("Thread details"),
            "the title row stays: {down}"
        );
        // Past the end, the next frame stops at the last row.
        for _ in 0..100 {
            press(&mut app, KeyCode::Char('j'));
        }
        let end = details_after(&mut app, 40, 12);
        assert!(end.contains("Recorded branch") && end.contains("127.0.0.1:9"));
        assert!(end.contains("more ↑") && !end.contains("more ↑↓"), "{end}");
        press(&mut app, KeyCode::Char('k'));
        assert!(details_after(&mut app, 40, 12).contains("more ↑↓"));
        for (code, expected) in [
            (KeyCode::Char('g'), &top),
            (KeyCode::Char('G'), &end),
            (KeyCode::Home, &top),
            (KeyCode::End, &end),
            (KeyCode::Up, &end),
            (KeyCode::Home, &top),
            (KeyCode::PageUp, &top),
        ] {
            press(&mut app, code);
            let text = details_after(&mut app, 40, 12);
            assert_eq!(&text == expected, code != KeyCode::Up, "{code:?}: {text}");
        }
        press(&mut app, KeyCode::PageDown);
        assert_ne!(details_after(&mut app, 40, 12), top);
        press(&mut app, KeyCode::PageUp);
        assert_eq!(details_after(&mut app, 40, 12), top);
        // The wheel over the dialog scrolls it.
        let drawn = app.details.drawn.expect("the dialog");
        let (x, y) = (drawn.area.x + 3, drawn.area.y + 3);
        assert!(mouse(&mut app, MouseEventKind::ScrollDown, x, y));
        assert_eq!(details_after(&mut app, 40, 12), down);
        assert!(mouse(&mut app, MouseEventKind::ScrollUp, x, y));
        assert_eq!(details_after(&mut app, 40, 12), top);
        assert_eq!(app.composer.text(), "");
    }

    #[test]
    fn the_pane_rewraps_the_transcript_and_keeps_the_reader_in_place() {
        let mut app = details_tui();
        app.focus = Focus::Transcript;
        cells(&mut app, 200, 30);
        for _ in 0..3 {
            press(&mut app, KeyCode::PageUp);
        }
        cells(&mut app, 200, 30);
        let wide = app.transcript_area.width;
        let (top, _) = app.anchor.clone().expect("scrolled up");
        let prepared = &app.open.as_ref().unwrap().prepared;
        let (made, versions) = (prepared.made, prepared.versions.clone());
        for open in [true, false] {
            alt(&mut app, 'b');
            assert_eq!(app.focus, Focus::Transcript, "the transcript keeps focus");
            cells(&mut app, 200, 30);
            assert_eq!(app.details.shown().is_some(), open);
            assert_eq!(app.transcript_area.width < wide, open);
            // Every block is wrapped again at the new width, from the blocks already made.
            let prepared = &app.open.as_ref().unwrap().prepared;
            assert_eq!(prepared.made, made, "a block was made again");
            assert_eq!(prepared.versions, versions);
            // The same answer is at the top, and the transcript is still scrolled up.
            assert_eq!(app.anchor.as_ref().map(|(id, _)| id), Some(&top));
            assert!(app.scroll.rows() > 0);
            assert_eq!(app.composer.text(), "draft");
        }
    }

    #[test]
    fn the_details_follow_another_thread_and_the_last_ones_never_come_back() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = details_tui();
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        assert!(details_after(&mut app, 200, 50).contains("feature-a"));
        let started = reads(&runtime);

        // Thread u opens before its watch sends anything. The shell's copy stands in.
        app.open_thread(OpenThread::new("u".into(), mpsc::unbounded_channel().1));
        let text = details_after(&mut app, 200, 50);
        for shown in [
            "Other",
            "Local checkout · other",
            "/repo/other",
            "None recorded",
        ] {
            assert!(text.contains(shown), "{shown:?} in {text}");
        }
        assert!(
            !text.contains("feature") && !text.contains("Demo"),
            "{text}"
        );

        // Late news of thread t changes nothing: the shell's update of it, and a projection of
        // it on the new thread's watch.
        let prepares = app.details.prepares;
        let moved = bound("t", "p", json!("/repo/feature-b"), json!("feature/b"));
        app.on_shell_event(thread_updated(2, moved));
        assert_eq!(details_after(&mut app, 200, 50), text);
        assert_eq!(app.details.prepares, prepares);
        let late = bound("t", "p", json!("/old"), json!("old"));
        let stale = json!({"kind": "snapshot", "snapshotSequence": 3,
            "projection": {"thread": late, "turnItems": []}});
        app.on_thread_event(WatchEvent::Item(stale));
        assert_eq!(details_after(&mut app, 200, 50), text);

        // A thread neither the shell nor its watch has sent is loading until its watch closes.
        app.open_thread(OpenThread::new("v".into(), mpsc::unbounded_channel().1));
        let text = details_after(&mut app, 200, 50);
        assert_eq!(text.matches("Loading…").count(), 3, "{text}");
        assert!(
            !text.contains("Other") && !text.contains("feature"),
            "{text}"
        );
        app.on_thread_event(WatchEvent::Failed("thread closed".into()));
        let text = details_after(&mut app, 200, 50);
        assert_eq!(text.matches("Unavailable").count(), 3, "{text}");
        assert!(!text.contains("Loading"), "{text}");
        // A watch that says it caught up without sending the thread ends the wait too.
        app.open_thread(OpenThread::new("w".into(), mpsc::unbounded_channel().1));
        assert_eq!(
            details_after(&mut app, 200, 50).matches("Loading…").count(),
            3
        );
        app.on_thread_event(WatchEvent::Item(json!({"kind": "synchronized"})));
        let text = details_after(&mut app, 200, 50);
        assert_eq!(text.matches("Unavailable").count(), 3, "{text}");
        // None of it asked T3 for anything or needs the tick.
        assert_eq!(reads(&runtime), started);
        assert!(!needs_tick(app.open.as_ref(), &app.sidebar));
    }

    #[test]
    fn live_changes_show_and_only_changes_that_can_matter_work_the_details_out() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = details_tui();
        let project = |sequence: u64, project: Value| {
            WatchEvent::Item(json!({"kind": "project.updated", "sequence": sequence,
                "project": project}))
        };
        // Closed, the details work nothing out, whatever comes.
        cells(&mut app, 200, 50);
        let renamed = json!({"id": "p", "title": "Demo renamed", "workspaceRoot": "/repo/demo"});
        app.on_shell_event(project(2, renamed));
        cells(&mut app, 200, 50);
        assert_eq!((app.details.prepares, app.details.wraps), (0, 0));
        let started = reads(&runtime);

        alt(&mut app, 'b');
        assert!(details_after(&mut app, 200, 50).contains("Demo renamed"));
        assert_eq!((app.details.prepares, app.details.wraps), (1, 1));
        // Frames, keys, an answer streaming in and news of another thread or project work
        // nothing out again and wrap nothing again.
        for _ in 0..3 {
            cells(&mut app, 200, 50);
        }
        for code in [KeyCode::Char('a'), KeyCode::Left, KeyCode::Right] {
            press(&mut app, code);
        }
        let answer = json!({"id": "a20", "type": "assistant_message", "ordinal": 20,
            "text": "Done"});
        app.on_thread_event(live(2, "turn-item.updated", answer));
        let other = bound("u", "q", json!("/repo/other-wt"), Value::Null);
        app.on_shell_event(thread_updated(3, other));
        let other = json!({"id": "q", "title": "Other renamed", "workspaceRoot": "/repo/o"});
        app.on_shell_event(project(4, other));
        app.on_shell_event(caught_up());
        cells(&mut app, 200, 50);
        assert_eq!((app.details.prepares, app.details.wraps), (1, 1));

        // The thread's watch moves it to another worktree and branch, and the details follow.
        let moved = bound(
            "t",
            "p",
            json!("/repo/.t3/worktrees/feature-b"),
            json!("feature/b"),
        );
        app.on_thread_event(live(3, "thread.meta-updated", moved));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Worktree · feature-b"), "{text}");
        assert!(
            text.contains("feature/b") && !text.contains("feature-a"),
            "{text}"
        );
        assert_eq!(app.details.prepares, 2);
        // Back in the local checkout, the project's folder names it, and follows the project.
        let local = bound("t", "p", Value::Null, json!("main"));
        app.on_thread_event(live(4, "thread.meta-updated", local));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Local checkout · demo") && text.contains("/repo/demo"));
        let moved = json!({"id": "p", "title": "Demo", "workspaceRoot": "/srv/demo-moved"});
        app.on_shell_event(project(5, moved));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Local checkout · demo-moved"), "{text}");
        assert!(text.contains("/srv/demo-moved"), "{text}");
        assert_eq!(app.details.prepares, 4);

        // Closed, they work nothing out until they open again, and then show what changed.
        alt(&mut app, 'b');
        let unbranched = bound("t", "p", Value::Null, Value::Null);
        app.on_thread_event(live(5, "thread.meta-updated", unbranched));
        cells(&mut app, 200, 50);
        assert_eq!(app.details.prepares, 4);
        alt(&mut app, 'b');
        assert!(details_after(&mut app, 200, 50).contains("None recorded"));
        assert_eq!(app.details.prepares, 5);
        // None of it asked T3 for anything or needs the tick.
        assert_eq!(reads(&runtime), started);
        assert!(!needs_tick(app.open.as_ref(), &app.sidebar));
    }

    #[test]
    fn the_threads_projection_comes_first_until_its_watch_closes() {
        let mut app = details_tui();
        // The watch's projection has a newer branch than the shell's copy.
        let ahead = bound(
            "t",
            "p",
            json!("/repo/.t3/worktrees/feature-a"),
            json!("feature/a-2"),
        );
        app.on_thread_event(live(2, "thread.meta-updated", ahead));
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        assert!(details_after(&mut app, 200, 50).contains("feature/a-2"));
        // Once the watch closes, the shell's copy, which still changes, stands in.
        app.on_thread_event(WatchEvent::Failed("thread closed".into()));
        let text = details_after(&mut app, 200, 50);
        assert!(
            text.contains("feature/a") && !text.contains("feature/a-2"),
            "{text}"
        );
        let local = bound("t", "p", Value::Null, json!("fix"));
        app.on_shell_event(thread_updated(2, local));
        let text = details_after(&mut app, 200, 50);
        assert!(
            text.contains("Local checkout · demo") && text.contains("fix"),
            "{text}"
        );
    }

    #[test]
    fn hostile_and_huge_fields_stay_inside_the_details_at_every_size() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let hostile = format!(
            "\u{1b}]52;c;cHduZWQ=\u{7}\u{1b}[2J{}\r\nevil",
            "👩\u{200d}💻e\u{301}名".repeat(20_000)
        );
        let root = format!("/r/{}", "\u{9b}x".repeat(200_000));
        let project = json!({"id": "p", "title": &hostile, "workspaceRoot": root});
        let worktree = format!("/w/{}\u{1b}[31m/last", "y".repeat(500_000));
        let mut thread = bound("t", "p", json!(worktree), json!(&hostile));
        // The title goes to the sidebar's card and the breadcrumb, as the project does.
        thread["title"] = json!(&hostile);
        let snapshot = json!({"snapshotSequence": 1, "projects": [project],
            "threads": [thread.clone()]});
        let shell = json!({"kind": "snapshot", "snapshot": snapshot});
        for sidebar_hidden in [false, true] {
            let settings = Settings {
                sidebar_hidden,
                ..Settings::default()
            };
            let mut app = tui(&settings);
            app.on_shell_event(WatchEvent::Item(shell.clone()));
            app.open_thread(open_bound(thread.clone()));
            cells(&mut app, 200, 50);
            alt(&mut app, 'b');
            for (width, height) in [(200, 50), (120, 36), (80, 24), (40, 12), (24, 8)] {
                let buffer = cells(&mut app, width, height);
                let drawn = app.details.drawn.expect("drawn");
                assert_eq!(drawn.area.intersection(app.screen), drawn.area);
                // No cell on the screen holds a control character: not the sidebar's, the
                // breadcrumb's or the details'.
                for y in 0..height {
                    for x in 0..width {
                        let symbol = buffer[(x, y)].symbol();
                        assert!(
                            !symbol.contains(char::is_control),
                            "{sidebar_hidden} {width}x{height} ({x}, {y}) {symbol:?}"
                        );
                    }
                }
                let text = details_text(&app, &buffer);
                assert!(text.contains("Thread details"), "{width}x{height}: {text}");
                let whole = text_in(&buffer, buffer.area);
                assert!(!whole.contains("evil"), "{whole}");
            }
            // The hostile text reached the breadcrumb, and the sidebar's card when it shows,
            // by its printable characters.
            let buffer = cells(&mut app, 200, 50);
            let title_row = text_in(&buffer, Rect::new(0, 1, 200, 1));
            assert!(title_row.contains('名'), "{title_row}");
            let sidebar = text_in(&buffer, app.sidebar.list);
            assert_eq!(sidebar.contains('名'), !sidebar_hidden, "{sidebar}");
            // The folder at the end of the long path still names the worktree.
            let text = details_text(&app, &buffer);
            assert!(text.contains("Worktree · last"), "{text}");
            // The values were worked out once, on the first frame that showed them.
            assert_eq!(app.details.prepares, 1);
        }
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn the_details_share_the_screen_with_menus_and_banners() {
        let mut app = tui(&Settings::default());
        app.on_shell_event(details_shell(vec![]));
        let thread = json!({"id": "t", "projectId": "p", "worktreePath": null, "branch": "main",
            "providerInstanceId": "codex", "activeProviderThreadId": "pt"});
        app.open_thread(codex_thread(
            "t",
            Some("Codex quit"),
            json!({"thread": thread}),
        ));
        app.composer.insert_str("draft");
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Local checkout · demo"), "{text}");
        // The error banner lies over the narrowed transcript, clear of the pane.
        let pane = app.details.drawn.expect("the pane").area;
        assert!(app.banner.showing());
        assert!(app.banner.area.width > 0);
        assert!(app.banner.area.right() <= app.transcript_area.right());
        assert!(app.transcript_area.right() < pane.x);
        // A menu opens over the conversation beside the pane, and the pane stays.
        app.picker = Some(mode_menu());
        cells(&mut app, 200, 50);
        assert!(app.picker_area.width > 0);
        assert!(app.picker_area.right() < pane.x);
        assert_eq!(app.details.drawn.map(|drawn| drawn.area), Some(pane));
        // Closing the pane leaves the menu open. A dialog closes it, as it takes the keyboard.
        alt(&mut app, 'b');
        assert!(app.picker.is_some());
        cells(&mut app, 80, 24);
        alt(&mut app, 'b');
        assert!(app.picker.is_none());
        assert_eq!(app.focus, Focus::Details);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.composer.text(), "draft");
    }

    // ---- linked pull requests ----

    /// Pull request `number` of krishhgg/t3term as T3 links it to a thread, open and synced,
    /// its checks passing and approved.
    fn linked(number: u64, title: &str) -> Value {
        json!({
            "host": "github.com",
            "repository": "krishhgg/t3term",
            "number": number,
            "url": format!("https://github.com/krishhgg/t3term/pull/{number}"),
            "source": "agent",
            "linkedAt": "2026-10-13T10:00:00.000Z",
            "snapshot": {
                "state": "open",
                "title": title,
                "headBranch": format!("feature-{number}"),
                "baseBranch": "master",
                "isDraft": false,
                "updatedAt": "2026-10-13T11:00:00.000Z",
                "syncedAt": "2026-10-13T11:59:00.000Z",
                "checksState": "passing",
                "reviewDecision": "approved"
            },
            "stack": null
        })
    }

    /// The feature thread `t` linked to `links`, as the shell lists it and its projection has
    /// it.
    fn linked_thread(links: Vec<Value>) -> Value {
        let mut thread = feature_thread();
        thread["pullRequests"] = json!(links);
        thread
    }

    /// `details_tui` on an environment that links pull requests to threads, with thread `t`
    /// linked to `links`.
    fn pulls_tui(links: Vec<Value>, sidebar_hidden: bool) -> App {
        let capabilities = json!({"threadPullRequests": true});
        pulls_tui_on(links, sidebar_hidden, capabilities)
    }

    /// `pulls_tui` on an environment with `capabilities`.
    fn pulls_tui_on(links: Vec<Value>, sidebar_hidden: bool, capabilities: Value) -> App {
        let mut client = Client::offline();
        client.runtime.capabilities = capabilities;
        let settings = Settings {
            sidebar_hidden,
            ..Settings::default()
        };
        let mut app = App::new(
            Arc::new(client),
            Theme::new(Depth::TrueColor),
            &settings,
            mpsc::unbounded_channel().0,
        );
        app.watch_setup = Box::new(|_: &str| mpsc::unbounded_channel().1);
        let other = bound("u", "q", Value::Null, Value::Null);
        let thread = linked_thread(links);
        app.on_shell_event(details_shell(vec![thread.clone(), other]));
        app.on_shell_event(caught_up());
        app.open_thread(open_bound(thread));
        app.composer.insert_str("draft");
        press(&mut app, KeyCode::Left);
        app
    }

    /// `linked` with every field the contract has, each long: a title of many words, a URL
    /// longer than t3term keeps, a long branch, a bot author, the lines it changes, a watch,
    /// and a GitHub stack of #31 under #32.
    fn long_linked(number: u64) -> Value {
        let mut link = linked(number, &"A long title ".repeat(60));
        let query = "q".repeat(6_000);
        let url = format!("https://github.com/krishhgg/t3term/pull/{number}?{query}");
        link["url"] = json!(url);
        link["snapshot"]["headBranch"] = json!(format!("feature/{}", "branch-".repeat(40)));
        link["snapshot"]["author"] = json!({"login": "renovate[bot]", "isBot": true});
        link["snapshot"]["additions"] = json!(1_234);
        link["snapshot"]["deletions"] = json!(56);
        link["watch"] = json!({"startedAt": "2026-10-13T10:00:00.000Z"});
        link["stack"] = json!({"kind": "native", "id": "s1",
            "layers": [{"number": 31}, {"number": 32}]});
        link
    }

    /// Draws a frame `width` columns by `height` rows and returns the rows of the details'
    /// body, the padding at their ends dropped.
    fn body_after(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let buffer = cells(app, width, height);
        let body = app.details.drawn.expect("the details").body;
        text_in(&buffer, body)
            .lines()
            .map(|row| row.trim_end().to_string())
            .collect()
    }

    /// Checks the status line at every width from 3 columns, where the dot and the cut
    /// connection start, to `widest`: it never runs past the width, and `esc` shows whole
    /// wherever there is room for it after the `restore` columns Ctrl+B takes.
    fn keys_fit(app: &App, widest: usize, esc: &str, restore: usize) {
        let least = "● live  ·  ".width() + restore + esc.width();
        for width in 3..=widest {
            let text = app.status_line(width, app.details.shown()).to_string();
            assert!(text.width() <= width, "{width}: {text:?}");
            if width >= least {
                assert!(text.contains(esc), "{width}: {text:?}");
            }
        }
    }

    /// Draws two frames `width` columns by `height` rows, checks the status line and returns
    /// the first frame's. The first frame draws it as the second does, so its keys follow how
    /// that frame draws the details and what they show, not what the frame before it drew.
    /// With `esc`, the details' keys show it. Without, the line offers no keys of theirs.
    fn first_status(app: &mut App, width: u16, height: u16, esc: Option<&str>) -> String {
        let row = Rect::new(0, height - 1, width, 1);
        let first = text_in(&cells(app, width, height), row);
        let layout = app.details.shown();
        let second = text_in(&cells(app, width, height), row);
        assert_eq!(first, second, "{width}x{height}, {layout:?}");
        match esc {
            Some(esc) => assert!(first.contains(esc), "{width}x{height}: {first:?}"),
            None => {
                for keys in ["Esc close", "Esc leave"] {
                    assert!(!first.contains(keys), "{width}x{height}: {first:?}");
                }
            }
        }
        first
    }

    #[test]
    fn the_pane_lists_the_threads_pull_requests_in_a_tab_the_keys_and_the_mouse_reach() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let links = vec![linked(31, "Bound titles"), linked(32, "Lineage")];
        let mut app = pulls_tui(links, false);
        app.focus = Focus::Transcript;
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        let started = reads(&runtime);
        // The thread's tab shows first, and the other counts the links.
        let text = details_after(&mut app, 200, 50);
        let scrolled = app.scroll.rows();
        assert!(text.contains("Thread  Pull requests 2"), "{text}");
        assert!(text.contains("Recorded branch"), "{text}");
        assert!(!text.contains("#31"), "{text}");

        // A click on the tab shows the links and gives the pane the keyboard.
        let drawn = app.details.drawn.expect("the pane");
        let tab = drawn.tabs[1];
        assert!(mouse(&mut app, CLICK, tab.x, tab.y));
        assert_eq!(app.details.tab(), details::Tab::PullRequests);
        assert_eq!(app.focus, Focus::Details);
        let buffer = cells(&mut app, 200, 50);
        let text = details_text(&app, &buffer);
        for shown in ["● #31 Bound titles", "● #32 Lineage", "2 open · 2 linked"] {
            assert!(text.contains(shown), "{shown:?} in {text}");
        }
        assert!(!text.contains("Recorded branch"), "{text}");
        let status = text_in(&buffer, Rect::new(0, 49, 200, 1));
        let hint = "↑↓ select · Enter details · ← thread";
        assert!(status.contains(hint), "{status}");

        // ↓ selects the second link and Enter shows its details in the list's place, under a
        // row back to the list. The composer, the draft and the transcript stay as they were.
        press(&mut app, KeyCode::Down);
        let text = details_after(&mut app, 200, 50);
        assert!(!text.contains("Linked by the agent"), "{text}");
        press(&mut app, KeyCode::Enter);
        let buffer = cells(&mut app, 200, 50);
        let text = details_text(&app, &buffer);
        for shown in ["← List", "2 of 2", "#32 Lineage", "Linked by the agent"] {
            assert!(text.contains(shown), "{shown:?} in {text}");
        }
        assert!(!text.contains("#31"), "{text}");
        let status = text_in(&buffer, Rect::new(0, 49, 200, 1));
        let hint = "↑↓/PgUp scroll · ← list · Esc leave";
        assert!(status.contains(hint), "{status}");
        for code in [KeyCode::Char('x'), KeyCode::Char('q'), KeyCode::Backspace] {
            press(&mut app, code);
        }
        assert_eq!(app.composer.text(), "draft");
        assert_eq!(app.scroll.rows(), scrolled);
        assert!(!app.quit);

        // ← goes back to the list, and from the list to the thread's tab. → shows the pull
        // requests again. `h` and `l` do as ← and → do.
        for (code, tab, viewing) in [
            (KeyCode::Left, details::Tab::PullRequests, false),
            (KeyCode::Left, details::Tab::Thread, false),
            (KeyCode::Right, details::Tab::PullRequests, false),
            (KeyCode::Enter, details::Tab::PullRequests, true),
            (KeyCode::Char('h'), details::Tab::PullRequests, false),
            (KeyCode::Char('h'), details::Tab::Thread, false),
            (KeyCode::Char('l'), details::Tab::PullRequests, false),
        ] {
            press(&mut app, code);
            assert_eq!(app.details.tab(), tab, "{code:?}");
            assert_eq!(app.details.pulls.viewing(), viewing, "{code:?}");
        }

        // A click on a link selects it, and a second shows its details. Until a frame draws
        // the pane again, a click does nothing.
        cells(&mut app, 200, 50);
        let drawn = app.details.drawn.expect("the pane");
        let (x, y) = (drawn.body.x + 1, drawn.body.y);
        assert!(mouse(&mut app, CLICK, x, y));
        assert!(!mouse(&mut app, CLICK, x, y));
        cells(&mut app, 200, 50);
        assert!(mouse(&mut app, CLICK, x, y));
        assert!(app.details.pulls.viewing());
        let text = details_after(&mut app, 200, 50);
        for shown in [
            "← List",
            "1 of 2",
            "#31 Bound titles",
            "Linked by the agent",
        ] {
            assert!(text.contains(shown), "{shown:?} in {text}");
        }
        assert!(!text.contains("#32"), "{text}");
        // The wheel scrolls the details, which all fit here. A click on the row above them
        // leads back to the list, with the link still selected.
        assert!(mouse(&mut app, MouseEventKind::ScrollDown, x, y));
        assert!(mouse(&mut app, MouseEventKind::ScrollUp, x, y));
        cells(&mut app, 200, 50);
        assert!(mouse(&mut app, CLICK, x, y));
        assert!(!app.details.pulls.viewing());
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("#32 Lineage"), "{text}");
        press(&mut app, KeyCode::Enter);
        assert!(details_after(&mut app, 200, 50).contains("1 of 2"));
        press(&mut app, KeyCode::Left);

        // Esc gives the keyboard back, and the pane keeps its tab.
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Transcript);
        assert!(app.details.open);
        assert_eq!(app.details.tab(), details::Tab::PullRequests);
        assert_eq!(app.composer.text(), "draft");
        // None of it asked T3 for anything or needs the tick.
        assert_eq!(reads(&runtime), started);
        assert!(!needs_tick(app.open.as_ref(), &app.sidebar));
    }

    #[test]
    fn a_dialog_shows_a_links_url_as_plain_text_and_its_tabs_take_clicks() {
        let links = vec![linked(31, "Bound titles"), linked(32, "Lineage")];
        let mut app = pulls_tui(links, false);
        app.focus = Focus::Transcript;
        cells(&mut app, 80, 24);
        alt(&mut app, 'b');
        assert_eq!(app.focus, Focus::Details);
        press(&mut app, KeyCode::Right);
        let buffer = cells(&mut app, 80, 24);
        let drawn = app.details.drawn.expect("the dialog");
        assert_eq!(drawn.layout, details::Layout::Dialog);
        // Beside the sidebar the status line has 41 columns for the keys, and the ones that
        // fit there show whole, Esc with them.
        let status = text_in(&buffer, Rect::new(0, 23, 80, 1));
        let hint = "↑↓ · Enter details · ← thread · Esc close";
        assert!(status.contains(hint), "{status}");
        // The dialog is as tall as the links and the footer, its title, its tabs and its
        // border.
        assert_eq!(drawn.area.height, 2 * 3 + 1 + 4);
        let url = "https://github.com/krishhgg/t3term/pull/31";
        assert!(!details_text(&app, &buffer).contains(url));
        // Enter shows the selected link's details in the list's place, and the dialog grows to
        // hold them.
        press(&mut app, KeyCode::Enter);
        let buffer = cells(&mut app, 80, 24);
        let text = details_text(&app, &buffer);
        for shown in [
            "← List",
            "1 of 2",
            "#31 Bound titles",
            url,
            "Linked by the agent",
        ] {
            assert!(text.contains(shown), "{shown:?} in {text}");
        }
        assert!(!text.contains("#32"), "{text}");
        let drawn = app.details.drawn.expect("the dialog");
        assert!(drawn.area.height > 2 * 3 + 1 + 4);
        let status = text_in(&buffer, Rect::new(0, 23, 80, 1));
        let hint = "↑↓/PgUp scroll · ← list · Esc close";
        assert!(status.contains(hint), "{status}");
        // A second Enter shows the list again, as ← does.
        press(&mut app, KeyCode::Enter);
        assert!(!details_after(&mut app, 80, 24).contains(url));
        press(&mut app, KeyCode::Enter);
        assert!(details_after(&mut app, 80, 24).contains(url));
        press(&mut app, KeyCode::Left);
        let text = details_after(&mut app, 80, 24);
        assert!(!text.contains(url), "{text}");
        assert!(text.contains("#32 Lineage"), "{text}");
        assert_eq!(app.details.tab(), details::Tab::PullRequests);

        // A click on the thread's tab shows it, and the dialog stays open with the keyboard.
        let drawn = app.details.drawn.expect("the dialog");
        let tab = drawn.tabs[0];
        assert!(mouse(&mut app, CLICK, tab.x, tab.y));
        assert_eq!(app.details.tab(), details::Tab::Thread);
        assert!(app.details.open);
        assert_eq!(app.focus, Focus::Details);
        let text = details_after(&mut app, 80, 24);
        assert!(text.contains("Recorded branch"), "{text}");
        // A click on the tab that shows changes nothing.
        let drawn = app.details.drawn.expect("the dialog");
        assert!(!mouse(&mut app, CLICK, drawn.tabs[0].x, drawn.tabs[0].y));
        press(&mut app, KeyCode::Esc);
        assert!(!app.details.open);
        assert_eq!(app.focus, Focus::Transcript);
        assert_eq!(app.composer.text(), "draft");
    }

    #[test]
    fn the_details_keys_shorten_to_the_room_there_is_and_esc_always_shows() {
        // After each step, the keys a dialog's status line offers at 80 columns, beside the
        // sidebar and without it: on the thread's tab, on the list and on a link's details.
        let steps = [
            (
                None,
                "↑↓/PgUp scroll · Esc close · → PRs",
                "↑↓/PgUp scroll · Esc close · → pull requests",
            ),
            (
                Some(KeyCode::Right),
                "↑↓ · Enter details · ← thread · Esc close",
                "↑↓ select · Enter details · ← thread · Esc close",
            ),
            (
                Some(KeyCode::Enter),
                "↑↓/PgUp scroll · ← list · Esc close",
                "↑↓/PgUp scroll · ← list · Esc close",
            ),
        ];
        for sidebar_hidden in [false, true] {
            let mut app = pulls_tui(vec![linked(31, "Bound titles")], sidebar_hidden);
            app.focus = Focus::Transcript;
            cells(&mut app, 80, 24);
            alt(&mut app, 'b');
            assert_eq!(app.focus, Focus::Details);
            // Ctrl+B leads the keys while the sidebar is hidden.
            let restore = if sidebar_hidden { 17 } else { 0 };
            for (key, beside, alone) in steps {
                if let Some(key) = key {
                    press(&mut app, key);
                }
                let buffer = cells(&mut app, 80, 24);
                let status = text_in(&buffer, Rect::new(0, 23, 80, 1));
                let keys = if sidebar_hidden { alone } else { beside };
                assert!(status.contains(keys), "{keys:?} in {status:?}");
                keys_fit(&app, 120, "Esc close", restore);
                // At 40 columns Esc shows whole without the sidebar. Beside it, 7 columns are
                // left, and the keys still start with Esc.
                let buffer = cells(&mut app, 40, 12);
                let status = text_in(&buffer, Rect::new(0, 11, 40, 1));
                let shown = if sidebar_hidden {
                    "Ctrl+B sidebar · Esc close"
                } else {
                    "live  ·  Esc"
                };
                assert!(status.contains(shown), "{shown:?} in {status:?}");
                // A screen 10 columns wide draws too.
                cells(&mut app, 10, 5);
            }
        }

        // A pane's keys say Esc leaves it, and shorten the same way.
        let mut app = pulls_tui(vec![linked(31, "Bound titles")], false);
        app.focus = Focus::Transcript;
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        cells(&mut app, 200, 50);
        let drawn = app.details.drawn.expect("the pane");
        assert!(mouse(&mut app, CLICK, drawn.tabs[0].x, drawn.tabs[0].y));
        assert_eq!(app.focus, Focus::Details);
        for key in [None, Some(KeyCode::Right), Some(KeyCode::Enter)] {
            if let Some(key) = key {
                press(&mut app, key);
            }
            let buffer = cells(&mut app, 200, 50);
            let status = text_in(&buffer, Rect::new(0, 49, 200, 1));
            let keys = app.details.keys(false).remove(0);
            assert!(status.contains(&keys), "{keys:?} in {status:?}");
            keys_fit(&app, 200, "Esc leave", 0);
        }
        assert!(app.details.pulls.viewing());
    }

    #[test]
    fn the_first_frame_at_a_new_size_offers_the_keys_for_how_it_draws_the_details() {
        for sidebar_hidden in [false, true] {
            // Beside the sidebar at 40 columns, the keys are cut a little after Esc.
            let narrow = if sidebar_hidden { "Esc close" } else { "Esc" };
            // On the thread's tab, on the list and on a link's details.
            for view in 0..3 {
                let mut app = pulls_tui(vec![linked(31, "Bound titles")], sidebar_hidden);
                app.focus = Focus::Transcript;
                cells(&mut app, 200, 50);
                // Alt+B opens a pane, which leaves the keyboard where it was.
                alt(&mut app, 'b');
                first_status(&mut app, 200, 50, None);
                // A click on the pane gives it the keyboard, and its keys say Esc leaves it.
                let drawn = app.details.drawn.expect("the pane");
                assert!(mouse(&mut app, CLICK, drawn.tabs[0].x, drawn.tabs[0].y));
                for code in [KeyCode::Right, KeyCode::Enter].into_iter().take(view) {
                    press(&mut app, code);
                }
                first_status(&mut app, 200, 50, Some("Esc leave"));
                assert_eq!(app.details.pulls.viewing(), view == 2);

                // The pane turns into a dialog at 80 and 40 columns, and into a pane again at
                // 200, with the keyboard all along. The first frame at each size offers the
                // keys for what it draws.
                for (width, height, esc) in [
                    (80, 24, "Esc close"),
                    (200, 50, "Esc leave"),
                    (40, 12, narrow),
                    (200, 50, "Esc leave"),
                    (80, 24, "Esc close"),
                ] {
                    assert!(app.on_terminal_event(Event::Resize(width, height)));
                    first_status(&mut app, width, height, Some(esc));
                    assert_eq!(app.focus, Focus::Details, "{width}x{height}");
                }
                // Esc closes the dialog and gives the keyboard back to the transcript, which had
                // it before the click.
                press(&mut app, KeyCode::Esc);
                assert!(!app.details.open);
                assert_eq!(app.focus, Focus::Transcript);
                first_status(&mut app, 80, 24, None);

                // Alt+B opens a dialog that takes the keyboard. A pane it turns into gives it
                // back on its first frame, and a dialog takes it again.
                alt(&mut app, 'b');
                assert_eq!(app.focus, Focus::Details);
                first_status(&mut app, 80, 24, Some("Esc close"));
                assert!(app.on_terminal_event(Event::Resize(200, 50)));
                first_status(&mut app, 200, 50, None);
                assert_eq!(app.focus, Focus::Transcript);
                assert!(app.on_terminal_event(Event::Resize(40, 12)));
                first_status(&mut app, 40, 12, Some(narrow));
                assert_eq!(app.focus, Focus::Details);
                // Alt+B closes it, and the first frame after offers the transcript's keys.
                alt(&mut app, 'b');
                assert_eq!(app.focus, Focus::Transcript);
                first_status(&mut app, 40, 12, None);
                assert_eq!(app.composer.text(), "draft");
            }
        }
    }

    #[test]
    fn the_first_frame_after_t3_changes_the_links_offers_the_keys_for_what_it_shows() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        // The pane, which a click gives the keyboard, then the dialog.
        for (width, height, esc) in [(200, 50, "Esc leave"), (80, 24, "Esc close")] {
            let size = format!("{width}x{height}");
            let both = vec![linked(31, "Bound titles"), linked(32, "Lineage")];
            let mut app = pulls_tui(both, false);
            app.focus = Focus::Transcript;
            cells(&mut app, width, height);
            alt(&mut app, 'b');
            cells(&mut app, width, height);
            let drawn = app.details.drawn.expect("the details");
            assert!(mouse(&mut app, CLICK, drawn.tabs[1].x, drawn.tabs[1].y));
            assert_eq!(app.focus, Focus::Details);
            let started = reads(&runtime);
            // #32's details show, and the keys lead back to the list.
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            let status = first_status(&mut app, width, height, Some(esc));
            assert!(status.contains("← list"), "{size}: {status:?}");
            let mut builds = app.details.pulls.builds;

            // The host syncs #32 as merged. A sync, a link and an unlink each reach the thread's
            // watch as `thread.pull-request-synced` (apps/server/src/orchestration-v2/
            // Orchestrator.ts:3188-3194 in the nightly), which carries the whole thread
            // (packages/contracts/src/orchestrationV2.ts:1665-1672). #32's details still show,
            // so the first frame after still offers the list.
            let mut merged = linked(32, "Lineage");
            merged["snapshot"]["state"] = json!("merged");
            let synced = linked_thread(vec![linked(31, "Bound titles"), merged.clone()]);
            app.on_thread_event(live(2, "thread.pull-request-synced", synced));
            let status = first_status(&mut app, width, height, Some(esc));
            assert!(status.contains("← list"), "{size}: {status:?}");
            let text = details_after(&mut app, width, height);
            assert!(text.contains("2 of 2"), "{size}: {text}");
            assert!(text.contains("Merged"), "{size}: {text}");
            // T3 unlinks #31, and the first frame still offers the list, where ← leads.
            let unlinked = linked_thread(vec![merged]);
            app.on_thread_event(live(3, "thread.pull-request-synced", unlinked));
            let status = first_status(&mut app, width, height, Some(esc));
            assert!(status.contains("← list"), "{size}: {status:?}");
            let text = details_after(&mut app, width, height);
            assert!(text.contains("1 of 1"), "{size}: {text}");
            builds += 2;
            assert_eq!(app.details.pulls.builds, builds, "{size}");
            press(&mut app, KeyCode::Left);
            assert!(!app.details.pulls.viewing(), "{size}");
            assert_eq!(app.details.tab(), details::Tab::PullRequests, "{size}");

            // T3 unlinks #32 while its details show, and links #31 again. The first frame after
            // shows the list, and its keys say ← goes to the thread's tab, as it then does.
            press(&mut app, KeyCode::Enter);
            cells(&mut app, width, height);
            let back = linked_thread(vec![linked(31, "Bound titles")]);
            app.on_thread_event(live(4, "thread.pull-request-synced", back));
            let status = first_status(&mut app, width, height, Some(esc));
            assert!(status.contains("← thread"), "{size}: {status:?}");
            assert!(!status.contains("← list"), "{size}: {status:?}");
            assert!(!app.details.pulls.viewing(), "{size}");
            let text = details_after(&mut app, width, height);
            assert!(text.contains("#31 Bound titles"), "{size}: {text}");
            assert!(!text.contains("← List"), "{size}: {text}");
            builds += 1;
            assert_eq!(app.details.pulls.builds, builds, "{size}");
            press(&mut app, KeyCode::Left);
            assert_eq!(app.details.tab(), details::Tab::Thread, "{size}");

            // The environment stops linking pull requests to threads while #31's details show.
            // The first frame after says so and offers the list's keys too.
            press(&mut app, KeyCode::Right);
            press(&mut app, KeyCode::Enter);
            let text = details_after(&mut app, width, height);
            assert!(text.contains("1 of 1"), "{size}: {text}");
            let off = json!({"threadPullRequests": false});
            app.on_config_event(config_snapshot(Vec::new(), off));
            let status = first_status(&mut app, width, height, Some(esc));
            assert!(status.contains("← thread"), "{size}: {status:?}");
            let text = details_after(&mut app, width, height);
            assert!(text.contains("Linked pull requests unavailable"), "{text}");
            assert!(!app.details.pulls.viewing(), "{size}");

            // A ← that comes before the frame does what the last frame offered. With #31's
            // details showing again, T3 unlinks it, and ← then leads to the list.
            let on = json!({"threadPullRequests": true});
            app.on_config_event(config_snapshot(Vec::new(), on));
            cells(&mut app, width, height);
            press(&mut app, KeyCode::Enter);
            let text = details_after(&mut app, width, height);
            assert!(text.contains("1 of 1"), "{size}: {text}");
            let none = linked_thread(Vec::new());
            app.on_thread_event(live(5, "thread.pull-request-synced", none));
            press(&mut app, KeyCode::Left);
            assert_eq!(app.details.tab(), details::Tab::PullRequests, "{size}");
            let status = first_status(&mut app, width, height, Some(esc));
            assert!(status.contains("← thread"), "{size}: {status:?}");
            let text = details_after(&mut app, width, height);
            assert!(text.contains("No linked pull requests"), "{size}: {text}");
            builds += 3;
            assert_eq!(app.details.pulls.builds, builds, "{size}");

            // None of it moved the keyboard, touched the draft or asked T3 for anything.
            assert_eq!(app.focus, Focus::Details, "{size}");
            assert_eq!(app.composer.text(), "draft");
            assert_eq!(reads(&runtime), started, "{size}");
        }
    }

    #[test]
    fn a_click_after_the_keys_or_the_wheel_lands_only_once_a_frame_shows_where_they_went() {
        let links: Vec<Value> = (1..=20).map(|number| linked(number, "Change")).collect();
        // The pane, then the dialog.
        for (width, height) in [(200, 50), (80, 24)] {
            let mut app = pulls_tui(links.clone(), false);
            app.focus = Focus::Transcript;
            cells(&mut app, width, height);
            alt(&mut app, 'b');
            cells(&mut app, width, height);
            let drawn = app.details.drawn.expect("the details");
            assert!(mouse(&mut app, CLICK, drawn.tabs[1].x, drawn.tabs[1].y));
            assert_eq!(app.details.tab(), details::Tab::PullRequests);
            assert_eq!(app.focus, Focus::Details);
            cells(&mut app, width, height);
            let drawn = app.details.drawn.expect("the details");
            let (x, y) = (drawn.body.x + 1, drawn.body.y);
            // The keys, a page and the wheel move the selection to the last link, past the
            // ones the frame showed. Until a frame draws the list again, a click on its first
            // row lands on nothing, rather than on the link that showed there.
            for code in [KeyCode::Down, KeyCode::PageDown, KeyCode::End] {
                press(&mut app, code);
            }
            assert!(mouse(&mut app, MouseEventKind::ScrollDown, x, y));
            for _ in 0..2 {
                mouse(&mut app, CLICK, x, y);
            }
            assert!(!app.details.pulls.viewing(), "{width}x{height}");
            assert_eq!(app.focus, Focus::Details);
            // So the last link is still selected, and Enter shows its details.
            cells(&mut app, width, height);
            press(&mut app, KeyCode::Enter);
            let text = details_after(&mut app, width, height);
            assert!(text.contains("20 of 20"), "{width}x{height}: {text}");

            // Back on the list, a click on the first row the frame drew selects the link it
            // shows, and a second after the next frame shows that link's details.
            press(&mut app, KeyCode::Left);
            let first = body_after(&mut app, width, height)[0].clone();
            let title = first.trim_start_matches("● ").to_string();
            assert!(title.starts_with('#'), "{first:?}");
            assert!(mouse(&mut app, CLICK, x, y));
            cells(&mut app, width, height);
            assert!(mouse(&mut app, CLICK, x, y));
            assert!(app.details.pulls.viewing());
            let rows = body_after(&mut app, width, height);
            assert_eq!(rows[1], title, "{width}x{height}");

            // A resize may move the details, so no click lands until a frame draws them, on
            // the row back to the list or on a tab.
            assert!(app.on_terminal_event(Event::Resize(width, height)));
            let drawn = app.details.drawn.expect("the details");
            assert!(!mouse(&mut app, CLICK, x, y));
            assert!(!mouse(&mut app, CLICK, drawn.tabs[0].x, drawn.tabs[0].y));
            assert!(app.details.pulls.viewing());
            assert_eq!(app.details.tab(), details::Tab::PullRequests);
            cells(&mut app, width, height);
            assert!(mouse(&mut app, CLICK, x, y));
            assert!(!app.details.pulls.viewing());
            assert_eq!(app.focus, Focus::Details);
            assert_eq!(app.composer.text(), "draft");
        }
    }

    #[test]
    fn long_details_scroll_to_their_last_row_by_key_and_wheel_and_lead_back_to_the_list() {
        let mut app = pulls_tui(vec![long_linked(32), long_linked(31)], false);
        app.focus = Focus::Transcript;
        cells(&mut app, 80, 24);
        alt(&mut app, 'b');
        press(&mut app, KeyCode::Right);
        cells(&mut app, 80, 24);
        // #31, the stack's bottom layer, comes first.
        press(&mut app, KeyCode::Enter);
        let text = details_after(&mut app, 80, 24);
        for shown in ["← List", "1 of 2", "#31 A long title", " more ↓ "] {
            assert!(text.contains(shown), "{shown:?} in {text}");
        }
        let drawn = app.details.drawn.expect("the dialog");
        let (x, y) = (drawn.body.x + 1, drawn.body.y);
        // The details end with the stack's note, which wraps to two rows here.
        let last = "below it.";

        // End and `G` reach the last row, and ↓ goes no further. The row back to the list
        // stays on top. Home and `g` go back to the first row.
        for (end, home) in [
            (KeyCode::End, KeyCode::Home),
            (KeyCode::Char('G'), KeyCode::Char('g')),
        ] {
            press(&mut app, end);
            let rows = body_after(&mut app, 80, 24);
            assert!(rows[0].starts_with("← List"), "{rows:#?}");
            assert_eq!(rows.last().map(String::as_str), Some(last), "{rows:#?}");
            press(&mut app, KeyCode::Down);
            let rows = body_after(&mut app, 80, 24);
            assert_eq!(rows.last().map(String::as_str), Some(last), "{rows:#?}");
            press(&mut app, home);
            let rows = body_after(&mut app, 80, 24);
            assert!(rows[1].starts_with("#31 A long title"), "{rows:#?}");
        }

        // PageDown passes every row on the way: the URL as far as t3term keeps it, with a note
        // that T3 sent more, the bot author and the watch.
        let mut seen = Vec::new();
        for _ in 0..10 {
            seen.extend(body_after(&mut app, 80, 24));
            press(&mut app, KeyCode::PageDown);
        }
        seen.extend(body_after(&mut app, 80, 24));
        assert_eq!(seen.last().map(String::as_str), Some(last));
        for shown in [
            "Cut short: T3 sent a URL longer than the 2,048",
            "renovate[bot] (bot)",
            "Watching: the agent wakes when checks finish,",
        ] {
            let found = seen.iter().any(|row| row == shown);
            assert!(found, "{shown:?} in {seen:#?}");
        }

        // The wheel reaches the last row too, from the top.
        press(&mut app, KeyCode::Home);
        let mut turns = 0;
        while body_after(&mut app, 80, 24).last().map(String::as_str) != Some(last) {
            assert!(mouse(&mut app, MouseEventKind::ScrollDown, x, y));
            turns += 1;
            assert!(turns < 200, "the wheel never reached the last row");
        }

        // The wheel moved the details, so a click on the row back to the list waits for the
        // frame that draws them. After it, the click leads back to the list, with #31 still
        // selected.
        assert!(mouse(&mut app, MouseEventKind::ScrollUp, x, y));
        assert!(!mouse(&mut app, CLICK, x, y));
        assert!(app.details.pulls.viewing());
        cells(&mut app, 80, 24);
        assert!(mouse(&mut app, CLICK, x, y));
        assert!(!app.details.pulls.viewing());
        let rows = body_after(&mut app, 80, 24);
        assert!(rows[0].starts_with("● #31 A long title"), "{rows:#?}");

        // ↓ selects #32, whose details end with the watch's note. ← leads back to the list
        // with #32 still selected, so Enter shows its details again.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::End);
        let rows = body_after(&mut app, 80, 24);
        assert!(rows[0].ends_with("2 of 2"), "{rows:#?}");
        let end = "someone comments, or the branch conflicts";
        assert_eq!(rows.last().map(String::as_str), Some(end), "{rows:#?}");
        press(&mut app, KeyCode::Left);
        assert!(!app.details.pulls.viewing());
        press(&mut app, KeyCode::Enter);
        let rows = body_after(&mut app, 80, 24);
        assert!(rows[0].ends_with("2 of 2"), "{rows:#?}");
        assert!(rows[1].starts_with("#32 A long title"), "{rows:#?}");
        assert_eq!(app.composer.text(), "draft");
    }

    #[test]
    fn a_scroll_after_end_or_past_the_end_wins_before_the_next_frame() {
        // The pane, then the dialog.
        for (width, height) in [(200, 50), (80, 24)] {
            let size = format!("{width}x{height}");
            let mut app = pulls_tui(vec![long_linked(32), long_linked(31)], false);
            app.focus = Focus::Transcript;
            cells(&mut app, width, height);
            alt(&mut app, 'b');
            cells(&mut app, width, height);
            let drawn = app.details.drawn.expect("the details");
            assert!(mouse(&mut app, CLICK, drawn.tabs[1].x, drawn.tabs[1].y));
            assert_eq!(app.focus, Focus::Details);
            cells(&mut app, width, height);
            // ↓ selects #32, the stack's top layer, and Enter shows its details.
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            let top = body_after(&mut app, width, height);
            assert!(top[0].ends_with("2 of 2"), "{size}: {top:#?}");
            let drawn = app.details.drawn.expect("the details");
            let (x, y) = (drawn.body.x + 1, drawn.body.y);

            // What End shows, and what PageUp, ↑ and the wheel up show with a frame between End
            // and them.
            press(&mut app, KeyCode::End);
            let bottom = body_after(&mut app, width, height);
            press(&mut app, KeyCode::PageUp);
            let page_up = body_after(&mut app, width, height);
            press(&mut app, KeyCode::End);
            cells(&mut app, width, height);
            press(&mut app, KeyCode::Up);
            let up = body_after(&mut app, width, height);
            press(&mut app, KeyCode::End);
            cells(&mut app, width, height);
            assert!(mouse(&mut app, MouseEventKind::ScrollUp, x, y));
            let wheel = body_after(&mut app, width, height);
            assert_ne!(up, bottom, "{size}");
            assert_ne!(page_up, up, "{size}");
            assert_eq!(wheel, up, "{size}");

            // With no frame between them, the key after End still moves from the last row.
            press(&mut app, KeyCode::Home);
            cells(&mut app, width, height);
            press(&mut app, KeyCode::End);
            press(&mut app, KeyCode::PageUp);
            assert_eq!(body_after(&mut app, width, height), page_up, "{size}");
            press(&mut app, KeyCode::Char('G'));
            press(&mut app, KeyCode::Up);
            assert_eq!(body_after(&mut app, width, height), up, "{size}");
            press(&mut app, KeyCode::End);
            assert!(mouse(&mut app, MouseEventKind::ScrollUp, x, y));
            assert_eq!(body_after(&mut app, width, height), wheel, "{size}");
            // Whichever of Home and End comes last wins.
            press(&mut app, KeyCode::End);
            press(&mut app, KeyCode::Home);
            assert_eq!(body_after(&mut app, width, height), top, "{size}");
            press(&mut app, KeyCode::Char('g'));
            press(&mut app, KeyCode::End);
            assert_eq!(body_after(&mut app, width, height), bottom, "{size}");

            // PageDown from the top reaches the last row. ↓, PageDown and the wheel past it stop
            // there, so ↑ and PageUp after them still move.
            press(&mut app, KeyCode::Home);
            let mut turns = 0;
            while body_after(&mut app, width, height) != bottom {
                press(&mut app, KeyCode::PageDown);
                turns += 1;
                assert!(turns < 50, "{size}: PageDown never reached the last row");
            }
            for _ in 0..5 {
                press(&mut app, KeyCode::Down);
                press(&mut app, KeyCode::PageDown);
                assert!(mouse(&mut app, MouseEventKind::ScrollDown, x, y));
            }
            press(&mut app, KeyCode::Up);
            assert_eq!(body_after(&mut app, width, height), up, "{size}");
            for _ in 0..5 {
                press(&mut app, KeyCode::PageDown);
            }
            press(&mut app, KeyCode::PageUp);
            assert_eq!(body_after(&mut app, width, height), page_up, "{size}");

            // Enter, End and ↑ before a frame first draws the details count from their last
            // row too.
            press(&mut app, KeyCode::Left);
            assert!(!app.details.pulls.viewing());
            cells(&mut app, width, height);
            press(&mut app, KeyCode::Enter);
            press(&mut app, KeyCode::End);
            press(&mut app, KeyCode::Up);
            assert_eq!(body_after(&mut app, width, height), up, "{size}");

            // ← leads back to the list with #32 still selected, so Enter shows its details from
            // their top.
            press(&mut app, KeyCode::Left);
            assert!(!app.details.pulls.viewing());
            press(&mut app, KeyCode::Enter);
            assert_eq!(body_after(&mut app, width, height), top, "{size}");
            assert_eq!(app.focus, Focus::Details);
            assert_eq!(app.composer.text(), "draft");
        }
    }

    #[test]
    fn hostile_and_a_thousand_links_stay_inside_the_details_at_every_size() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let hostile = format!(
            "\u{1b}]52;c;cHduZWQ=\u{7}\u{1b}[2J{}\r\nevil",
            "👩\u{200d}💻e\u{301}名".repeat(20_000)
        );
        // Short enough in bytes to be read, and longer in characters than a row keeps.
        let mut first = linked(1, &hostile);
        first["repository"] = json!(format!("o/{}\u{9b}x", "漢".repeat(300)));
        let url = format!("https://github.com/{}", "\u{1b}u".repeat(100_000));
        first["url"] = json!(url);
        first["snapshot"]["headBranch"] = json!(&hostile);
        let mut links = vec![first];
        links.extend((2..=1_200).map(|number| linked(number, "A change")));
        for sidebar_hidden in [false, true] {
            let mut app = pulls_tui(links.clone(), sidebar_hidden);
            cells(&mut app, 200, 50);
            alt(&mut app, 'b');
            app.details.show(details::Tab::PullRequests);
            // The list, then the first link's details in its place.
            for viewing in [false, true] {
                for (width, height) in [(200, 50), (120, 36), (80, 24), (40, 12)] {
                    let buffer = cells(&mut app, width, height);
                    let drawn = app.details.drawn.expect("drawn");
                    assert_eq!(drawn.area.intersection(app.screen), drawn.area);
                    for part in [drawn.close, drawn.tabs[0], drawn.tabs[1], drawn.body] {
                        assert_eq!(part.intersection(drawn.area), part, "{width}x{height}");
                    }
                    // No cell on the screen holds a control character.
                    for y in 0..height {
                        for x in 0..width {
                            let symbol = buffer[(x, y)].symbol();
                            assert!(
                                !symbol.contains(char::is_control),
                                "{sidebar_hidden} {width}x{height} ({x}, {y}) {symbol:?}"
                            );
                        }
                    }
                    let text = details_text(&app, &buffer);
                    assert!(text.contains("#1 "), "{width}x{height}: {text}");
                    if viewing {
                        assert!(text.contains("1 of 1000"), "{width}x{height}: {text}");
                    }
                    let whole = text_in(&buffer, buffer.area);
                    assert!(!whole.contains("evil"), "{whole}");
                }
                assert_eq!(app.details.enter(), None);
                assert_eq!(app.details.pulls.viewing(), !viewing);
            }
            // The links were read once for every frame at every size, a thousand of them.
            assert_eq!(app.details.pulls.builds, 1);
            assert_eq!(app.details.pulls.count(), Some(1_000));
            assert_eq!(app.composer.text(), "draft");
        }
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn only_news_of_the_thread_works_its_links_out_and_another_thread_starts_fresh() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = pulls_tui(vec![linked(31, "Bound titles")], false);
        // Closed, the details read no links, whatever comes.
        cells(&mut app, 200, 50);
        let more = linked_thread(vec![linked(31, "Bound titles"), linked(33, "Later")]);
        app.on_thread_event(live(2, "thread.meta-updated", more));
        cells(&mut app, 200, 50);
        assert_eq!(app.details.pulls.builds, 0);
        let started = reads(&runtime);

        alt(&mut app, 'b');
        app.details.show(details::Tab::PullRequests);
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("#33 Later"), "{text}");
        assert!(text.contains("Pull requests 2"), "{text}");
        assert_eq!(app.details.pulls.builds, 1);
        // Frames, an answer streaming in and news of another thread read nothing again.
        for _ in 0..3 {
            cells(&mut app, 200, 50);
        }
        let answer = json!({"id": "a20", "type": "assistant_message", "ordinal": 20,
            "text": "Done"});
        app.on_thread_event(live(3, "turn-item.updated", answer));
        let other = bound("u", "q", json!("/repo/other-wt"), Value::Null);
        app.on_shell_event(thread_updated(3, other));
        cells(&mut app, 200, 50);
        assert_eq!(app.details.pulls.builds, 1);

        // The thread's watch drops a link, and the list follows.
        app.focus = Focus::Details;
        press(&mut app, KeyCode::Down);
        let fewer = linked_thread(vec![linked(31, "Bound titles")]);
        app.on_thread_event(live(4, "thread.meta-updated", fewer));
        let text = details_after(&mut app, 200, 50);
        assert!(!text.contains("#33"), "{text}");
        assert!(text.contains("#31"), "{text}");
        assert!(text.contains("Pull requests 1"), "{text}");
        assert_eq!(app.details.pulls.builds, 2);

        // Another thread starts at its own list, and late news of `t` never brings its links
        // back.
        app.open_thread(OpenThread::new("u".into(), mpsc::unbounded_channel().1));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("No linked pull requests"), "{text}");
        assert!(!text.contains("#31"), "{text}");
        assert!(!text.contains("Pull requests 1"), "{text}");
        let late = linked_thread(vec![linked(34, "Stale")]);
        app.on_shell_event(thread_updated(4, late.clone()));
        let stale = json!({"kind": "snapshot", "snapshotSequence": 3,
            "projection": {"thread": late, "turnItems": []}});
        app.on_thread_event(WatchEvent::Item(stale));
        let after = details_after(&mut app, 200, 50);
        assert_eq!(after, text);
        assert_eq!(app.composer.text(), "draft");
        assert_eq!(reads(&runtime), started);
        assert!(!needs_tick(app.open.as_ref(), &app.sidebar));
    }

    #[test]
    fn the_links_wait_for_the_thread_and_need_an_environment_that_links_them() {
        // The environment's capabilities lack `threadPullRequests`, so the list is unavailable
        // and counts nothing, whatever the thread holds.
        let mut app = details_tui();
        let thread = linked_thread(vec![linked(31, "Bound titles")]);
        app.on_shell_event(thread_updated(2, thread.clone()));
        app.open_thread(open_bound(thread));
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        app.details.show(details::Tab::PullRequests);
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Linked pull requests unavailable"), "{text}");
        assert!(!text.contains("Pull requests 1"), "{text}");
        assert!(!text.contains("#31"), "{text}");

        // A thread neither the shell nor its watch has sent is loading until its watch closes.
        let mut app = pulls_tui(Vec::new(), false);
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        app.details.show(details::Tab::PullRequests);
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("No linked pull requests"), "{text}");
        app.open_thread(OpenThread::new("v".into(), mpsc::unbounded_channel().1));
        assert!(details_after(&mut app, 200, 50).contains("Loading…"));
        app.on_thread_event(WatchEvent::Failed("thread closed".into()));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Unavailable"), "{text}");
        assert!(!text.contains("Loading"), "{text}");
    }

    // ---- lineage ----

    /// Thread `id` in Demo, a fork or a subagent of thread `parent` by `relationship`.
    fn related(id: &str, parent: &str, relationship: &str) -> Value {
        let mut thread = bound(id, "p", Value::Null, Value::Null);
        let lineage = json!({"parentThreadId": parent, "relationshipToParent": relationship});
        thread["lineage"] = lineage;
        thread
    }

    /// An agent thread `t` delegated to, on thread `child`, or on none yet.
    fn subagent(id: &str, child: Value, status: &str) -> Value {
        json!({"id": id, "threadId": "t", "childThreadId": child, "status": status,
            "title": format!("Agent {id}")})
    }

    /// The TUI with thread `t`, a fork of `up`, open, and its details open as a pane with the
    /// keyboard. The shell lists `t`'s fork `f` and the thread `a` of its running agent. `t`'s
    /// projection has agent `s1` on `a`, and `s2`, done, on a thread the shell doesn't list.
    /// The composer has a draft with its cursor before the last letter.
    fn lineage_tui() -> App {
        lineage_on(tui(&Settings::default()), None)
    }

    /// `lineage_tui` built on `app`, with `t` linked to the pull requests `links` when there
    /// are any.
    fn lineage_on(mut app: App, links: Option<Vec<Value>>) -> App {
        let mut current = feature_thread();
        current["lineage"] = json!({"parentThreadId": "up", "relationshipToParent": "fork"});
        if let Some(links) = links {
            current["pullRequests"] = json!(links);
        }
        let mut agent = related("a", "t", "subagent");
        agent["activityRunStatus"] = json!("running");
        let threads = vec![
            bound("up", "p", Value::Null, Value::Null),
            current.clone(),
            related("f", "t", "fork"),
            agent,
        ];
        app.on_shell_event(details_shell(threads));
        app.on_shell_event(caught_up());
        app.open_thread(open_bound(current));
        let agents = [
            subagent("s1", json!("a"), "running"),
            subagent("s2", json!("gone"), "completed"),
        ];
        for (sequence, agent) in (2..).zip(agents) {
            app.on_thread_event(live(sequence, "subagent.updated", agent));
        }
        app.composer.insert_str("draft");
        press(&mut app, KeyCode::Left);
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        cells(&mut app, 200, 50);
        let order = focus_after(&mut app, &[KeyCode::Tab; 2]);
        assert_eq!(order, [Focus::Transcript, Focus::Details]);
        app
    }

    /// The thread open in `app`.
    fn open_id(app: &App) -> Option<&str> {
        app.open.as_ref().map(|open| open.id.as_str())
    }

    /// Draws a frame 200 columns by 50 rows and returns its status line.
    fn status_after(app: &mut App) -> String {
        let buffer = cells(app, 200, 50);
        text_in(&buffer, Rect::new(0, 49, 200, 1))
    }

    /// Draws a frame `width` columns by `height` rows and returns the cell in the middle of
    /// the details' row that shows `needle`.
    fn details_row(app: &mut App, width: u16, height: u16, needle: &str) -> (u16, u16) {
        let buffer = cells(app, width, height);
        let area = app.details.drawn.expect("the details").area;
        let text = text_in(&buffer, area);
        let line = text
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no row shows {needle:?} in {text}"));
        (area.x + area.width / 2, area.y + line as u16)
    }

    #[test]
    fn lineage_rows_open_their_threads_and_the_pane_keeps_the_keyboard_and_the_draft() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = lineage_tui();
        let started = reads(&runtime);
        let text = details_after(&mut app, 200, 50);
        for shown in [
            "Lineage · 1 running",
            "↑ Thread up",
            "↳ Thread f",
            "◆ Thread a",
            "› Previous agents (1)",
        ] {
            assert!(text.contains(shown), "{shown:?} in {text}");
        }
        assert!(!text.contains("Agent s2"), "{text}");
        let status = status_after(&mut app);
        assert!(status.contains("↑↓ choose · PgUp"), "{status}");

        // ↓ chooses each row in turn, and the status line says what Enter does with it.
        for action in [
            "Enter open parent thread",
            "Enter open fork",
            "Enter open subagent",
            "Enter show previous agents",
        ] {
            press(&mut app, KeyCode::Down);
            let status = status_after(&mut app);
            assert!(status.contains(action), "{status}");
        }
        // Enter opens Previous agents, which shows the agent whose thread the shell doesn't
        // list. Enter on it only says it can't open.
        press(&mut app, KeyCode::Enter);
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("◆ Agent s2"), "{text}");
        press(&mut app, KeyCode::Down);
        let status = status_after(&mut app);
        assert!(status.contains("Subagent unavailable"), "{status}");
        press(&mut app, KeyCode::Enter);
        let status = status_after(&mut app);
        assert!(
            status.contains("This related thread is unavailable."),
            "{status}"
        );
        assert_eq!(open_id(&app), Some("t"));
        assert_eq!(reads(&runtime), started);

        // ↑ goes back a row, and the wheel leaves none chosen.
        press(&mut app, KeyCode::Up);
        let status = status_after(&mut app);
        assert!(status.contains("Enter hide previous agents"), "{status}");
        let pane = app.details.drawn.expect("the pane").area;
        let (x, y) = (pane.x + 2, pane.y + 2);
        assert!(mouse(&mut app, MouseEventKind::ScrollDown, x, y));
        assert_eq!(app.details.selected(), None);
        let status = status_after(&mut app);
        assert!(status.contains("↑↓ choose · PgUp"), "{status}");

        // ↓ chooses the first row again, and Enter opens the parent with a watch of its own.
        // The pane keeps the keyboard, the sidebar highlights the parent, and its Lineage
        // has `t` as its fork and none of `t`'s own.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(open_id(&app), Some("up"));
        assert_eq!(reads(&runtime), started + 1);
        assert_eq!(app.focus, Focus::Details);
        assert_eq!(app.sidebar.selected_thread_id(), Some("up"));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("↳ Thread t"), "{text}");
        assert!(!text.contains("Thread f"), "{text}");

        // From the parent, ↓ and Enter open `t` again. Esc gives the keyboard back to the
        // transcript, which had it before the pane, and the draft kept its cursor.
        press(&mut app, KeyCode::Down);
        let status = status_after(&mut app);
        assert!(status.contains("Enter open fork"), "{status}");
        press(&mut app, KeyCode::Enter);
        assert_eq!(open_id(&app), Some("t"));
        assert_eq!(reads(&runtime), started + 2);
        assert_eq!(app.focus, Focus::Details);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Transcript);
        assert!(app.details.open);
        app.focus = Focus::Composer;
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.composer.text(), "drafxt");
    }

    #[test]
    fn a_click_on_a_lineage_row_acts_on_the_row_the_last_frame_drew() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = lineage_tui();
        let started = reads(&runtime);
        // A click on Previous agents opens the group. A second click before a frame draws it
        // does nothing, so the group stays open.
        let (x, y) = details_row(&mut app, 200, 50, "› Previous agents (1)");
        assert!(mouse(&mut app, CLICK, x, y));
        assert!(!mouse(&mut app, CLICK, x, y));
        // A click on the agent whose thread the shell doesn't list only says it can't open.
        let (x, y) = details_row(&mut app, 200, 50, "◆ Agent s2");
        assert!(mouse(&mut app, CLICK, x, y));
        let status = status_after(&mut app);
        assert!(
            status.contains("This related thread is unavailable."),
            "{status}"
        );
        assert_eq!(open_id(&app), Some("t"));
        assert_eq!(reads(&runtime), started);
        // A click on the fork opens it, and the pane keeps the keyboard.
        let (x, y) = details_row(&mut app, 200, 50, "↳ Thread f");
        assert!(mouse(&mut app, CLICK, x, y));
        assert_eq!(open_id(&app), Some("f"));
        assert_eq!(app.focus, Focus::Details);
        assert_eq!(app.sidebar.selected_thread_id(), Some("f"));
        assert_eq!(reads(&runtime), started + 1);

        // In a dialog, a click on the fork's parent at the end of the text opens it the same
        // way.
        app.on_terminal_event(Event::Resize(80, 24));
        cells(&mut app, 80, 24);
        assert_eq!(app.details.shown(), Some(details::Layout::Dialog));
        press(&mut app, KeyCode::End);
        let (x, y) = details_row(&mut app, 80, 24, "↑ Thread t");
        assert!(mouse(&mut app, CLICK, x, y));
        assert_eq!(open_id(&app), Some("t"));
        assert_eq!(app.focus, Focus::Details);
        assert_eq!(reads(&runtime), started + 2);
        assert_eq!(app.composer.text(), "draft");
    }

    #[test]
    fn a_hundred_and_twenty_four_agents_page_by_twelve_and_stay_inside_at_every_size() {
        let mut app = lineage_tui();
        // 123 more agents finish, 7 of them failed, with titles that try to clear the screen.
        for i in 0..123_u64 {
            let failed = i.is_multiple_of(18);
            let status = if failed { "failed" } else { "completed" };
            let mut agent = subagent(&format!("p{i}"), json!(format!("c{i}")), status);
            agent["title"] = json!(format!("\u{1b}[2J\u{9b}31m{}", "名".repeat(300)));
            app.on_thread_event(live(4 + i, "subagent.updated", agent));
        }
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("› Previous agents (124)"), "{text}");
        assert!(text.contains("7 failed"), "{text}");
        // The 123 events worked Lineage out once, on the frame after them.
        assert_eq!(app.details.lineage_prepares, 2);

        // ↓ reaches the group's heading, Enter opens it with its first six, and each Enter on
        // Show more adds twelve, then the last ten.
        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Enter);
        cells(&mut app, 200, 50);
        for _ in 0..7 {
            press(&mut app, KeyCode::Down);
        }
        for _ in 0..9 {
            let status = status_after(&mut app);
            assert!(status.contains("Enter show 12 more"), "{status}");
            press(&mut app, KeyCode::Enter);
        }
        let status = status_after(&mut app);
        assert!(status.contains("Enter show 10 more"), "{status}");
        press(&mut app, KeyCode::Enter);
        // With all 124 showing, the choice moved to the last of them.
        let status = status_after(&mut app);
        assert!(status.contains("Subagent unavailable"), "{status}");
        // Opening the group and each of its ten pages worked Lineage out once more.
        assert_eq!(app.details.lineage_prepares, 13);

        // At every size, with the sidebar and without, the details stay on the screen and no
        // cell holds a control character.
        for hide in [false, true] {
            if hide {
                ctrl_b(&mut app);
            }
            for (width, height) in [(200, 50), (120, 36), (80, 24), (40, 12)] {
                let buffer = cells(&mut app, width, height);
                let drawn = app.details.drawn.expect("drawn");
                assert_eq!(drawn.area.intersection(app.screen), drawn.area);
                for y in 0..height {
                    for x in 0..width {
                        let symbol = buffer[(x, y)].symbol();
                        assert!(
                            !symbol.contains(char::is_control),
                            "{hide} {width}x{height} ({x}, {y}) {symbol:?}"
                        );
                    }
                }
            }
        }
        assert_eq!(app.details.lineage_prepares, 13);
    }

    #[test]
    fn lineage_is_worked_out_again_only_after_events_that_can_change_it() {
        let mut app = lineage_tui();
        assert_eq!(app.details.lineage_prepares, 1);
        // Frames, keys, an answer streaming in and news of an unrelated thread or a project
        // work nothing out again.
        for _ in 0..3 {
            cells(&mut app, 200, 50);
        }
        for code in [KeyCode::Down, KeyCode::Up, KeyCode::Char('x')] {
            press(&mut app, code);
        }
        let answer = json!({"id": "a20", "type": "assistant_message", "ordinal": 20,
            "text": "Done"});
        app.on_thread_event(live(4, "turn-item.updated", answer));
        let unrelated = bound("v", "q", Value::Null, Value::Null);
        app.on_shell_event(thread_updated(2, unrelated));
        let project = json!({"kind": "project.updated", "sequence": 3,
            "project": {"id": "q", "title": "Other renamed", "workspaceRoot": "/repo/o"}});
        app.on_shell_event(WatchEvent::Item(project));
        app.on_shell_event(caught_up());
        cells(&mut app, 200, 50);
        assert_eq!(app.details.lineage_prepares, 1);

        // The agent's thread ends its run with a failure, which outranks the agent's own
        // status, so the agent moves to Previous agents.
        let mut ended = related("a", "t", "subagent");
        ended["activityRunStatus"] = json!("failed");
        app.on_shell_event(thread_updated(4, ended));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("› Previous agents (2)"), "{text}");
        assert!(text.contains("1 failed"), "{text}");
        assert!(!text.contains("1 running"), "{text}");
        assert_eq!(app.details.lineage_prepares, 2);
        // A new fork of the thread shows.
        app.on_shell_event(thread_updated(5, related("g", "t", "fork")));
        assert!(details_after(&mut app, 200, 50).contains("↳ Thread g"));
        assert_eq!(app.details.lineage_prepares, 3);
        // An agent with no thread yet counts as running.
        let waiting = subagent("s3", Value::Null, "running");
        app.on_thread_event(live(5, "subagent.updated", waiting));
        assert!(details_after(&mut app, 200, 50).contains("Lineage · 1 running"));
        assert_eq!(app.details.lineage_prepares, 4);

        // Closed, the details work nothing out, and they catch up when they open again.
        alt(&mut app, 'b');
        app.on_shell_event(thread_updated(6, related("h", "t", "fork")));
        cells(&mut app, 200, 50);
        assert_eq!(app.details.lineage_prepares, 4);
        alt(&mut app, 'b');
        assert!(details_after(&mut app, 200, 50).contains("↳ Thread h"));
        assert_eq!(app.details.lineage_prepares, 5);
    }

    #[test]
    fn a_closed_watch_leaves_lineage_to_the_shell() {
        let mut app = lineage_tui();
        // Another agent with no thread yet runs, and one runs on a thread the shell doesn't
        // list.
        let unseen = subagent("s3", Value::Null, "running");
        app.on_thread_event(live(4, "subagent.updated", unseen));
        let lost = subagent("s4", json!("lost"), "running");
        app.on_thread_event(live(5, "subagent.updated", lost));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Lineage · 3 running"), "{text}");
        let prepared = app.details.lineage_prepares;

        // The watch closes, and Lineage is worked out again once. The agents only the
        // projection knows of no longer count as running, since t3term won't hear them end,
        // and the one on a thread the shell doesn't list has no status.
        app.on_thread_event(WatchEvent::Failed("The thread's watch closed.".into()));
        let text = details_after(&mut app, 200, 50);
        assert_eq!(app.details.lineage_prepares, prepared + 1);
        assert!(text.contains("Lineage · 1 running"), "{text}");
        let row = text
            .lines()
            .find(|line| line.contains("◆ Agent s4"))
            .unwrap_or_default();
        assert!(row.contains("Unavailable"), "{text}");

        // The shell still hears the agent's thread, which goes idle. Its agent still says it
        // runs, which counted while the watch was open, but now the thread's copy goes first,
        // so it joins the agent that finished under Previous agents.
        app.on_shell_event(thread_updated(2, related("a", "t", "subagent")));
        let text = details_after(&mut app, 200, 50);
        assert_eq!(app.details.lineage_prepares, prepared + 2);
        assert!(text.contains("› Previous agents (2)"), "{text}");
        assert!(!text.contains("Lineage ·"), "{text}");
        assert!(text.contains("◆ Agent s4"), "{text}");
    }

    #[test]
    fn the_parent_still_opens_after_the_shell_drops_the_open_thread() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = lineage_tui();
        let started = reads(&runtime);
        // T3 archives `t`, so the shell drops it. Its watch goes on, and its projection still
        // has it as a fork of `up`, which the shell lists.
        let removed = json!({"kind": "thread.removed", "sequence": 2, "threadId": "t"});
        app.on_shell_event(WatchEvent::Item(removed));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("↑ Thread up"), "{text}");
        assert!(text.contains("↳ Thread f"), "{text}");
        // The watch closes too, and the parent stays.
        app.on_thread_event(WatchEvent::Failed("The thread's watch closed.".into()));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("↑ Thread up"), "{text}");

        // ↓ chooses the parent, and Enter opens it with a watch of its own. The pane keeps the
        // keyboard, and the sidebar highlights the parent.
        press(&mut app, KeyCode::Down);
        let status = status_after(&mut app);
        assert!(status.contains("Enter open parent thread"), "{status}");
        press(&mut app, KeyCode::Enter);
        assert_eq!(open_id(&app), Some("up"));
        assert_eq!(reads(&runtime), started + 1);
        assert_eq!(app.focus, Focus::Details);
        assert_eq!(app.sidebar.selected_thread_id(), Some("up"));
    }

    /// The TUI with thread `t` open and highlighted in the sidebar's inbox, and `other`, a
    /// thread related to `t`, on a server that settles threads. Working, Done and Settled are
    /// closed, as the saved settings have them. The details are open as a pane with the
    /// keyboard, and the composer has a draft with its cursor before the last letter.
    fn closed_shelves_lineage_tui(other: Value) -> App {
        Settings::update(|saved| *saved = shelves_closed());
        let mut client = Client::offline();
        client.runtime.capabilities = json!({"threadSettlement": true});
        let mut app = App::new(
            Arc::new(client),
            Theme::new(Depth::TrueColor),
            &shelves_closed(),
            mpsc::unbounded_channel().0,
        );
        app.watch_setup = Box::new(|_: &str| mpsc::unbounded_channel().1);
        // A finished run with no visit keeps `t` in the inbox, which doesn't fold.
        let mut current = bound("t", "p", Value::Null, Value::Null);
        current["status"] = json!("completed");
        app.on_shell_event(details_shell(vec![current.clone(), other]));
        app.on_shell_event(caught_up());
        app.open_thread(open_bound(current));
        assert!(app.sidebar.select("t"));
        app.composer.insert_str("draft");
        press(&mut app, KeyCode::Left);
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        cells(&mut app, 200, 50);
        let order = focus_after(&mut app, &[KeyCode::Tab; 2]);
        assert_eq!(order, [Focus::Transcript, Focus::Details]);
        app
    }

    #[test]
    fn a_related_thread_on_a_closed_shelf_opens_listed_there_and_highlighted() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut working = related("w", "t", "fork");
        working["status"] = json!("running");
        let mut done = related("d", "t", "fork");
        done["status"] = json!("completed");
        done["latestRunCompletedAt"] = json!("2026-10-08T11:00:00.000Z");
        done["lastVisitedAt"] = json!("2026-10-08T10:00:00.000Z");
        let mut settled = related("s", "t", "fork");
        settled["settledOverride"] = json!("settled");
        for (shelf, fork) in [("Working", working), ("Done", done), ("Settled", settled)] {
            let id = str_of(&fork, "id").to_string();
            let mut app = closed_shelves_lineage_tui(fork);
            let started = reads(&runtime);
            // The fork's shelf is closed, so the sidebar lists only its heading.
            let heading = format!("{shelf} (1) ─");
            let rows = screen(&mut app, 200, 50);
            let at = row_showing(&rows, &heading) as usize;
            assert!(rows[at].contains("─ ›"), "{shelf}: {rows:#?}");
            assert!(!clickable(&app).contains(&id), "{shelf}: {rows:#?}");

            // ↓ chooses the fork, and Enter opens it with a watch of its own. The shelf lists
            // it as the open thread, and the sidebar highlights it at once.
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            assert_eq!(open_id(&app), Some(id.as_str()), "{shelf}");
            assert_eq!(
                app.sidebar.selected_thread_id(),
                Some(id.as_str()),
                "{shelf}"
            );
            assert_eq!(reads(&runtime), started + 1, "{shelf}");
            assert_eq!(app.focus, Focus::Details, "{shelf}");

            // The first frame draws its row under the heading, which stays closed.
            let rows = screen(&mut app, 200, 50);
            let at = row_showing(&rows, &heading) as usize;
            assert!(rows[at].contains("─ ›"), "{shelf}: {rows:#?}");
            assert!(clickable(&app).contains(&id), "{shelf}: {rows:#?}");
            assert_eq!(
                app.sidebar.selected_thread_id(),
                Some(id.as_str()),
                "{shelf}"
            );
            let saved = crate::settings::saved_in_test();
            assert!(!saved.sidebar_working_shelf_expanded, "{shelf}");
            assert!(!saved.sidebar_done_shelf_expanded, "{shelf}");
            assert!(!saved.sidebar_settled_shelf_expanded, "{shelf}");
            assert_eq!(reads(&runtime), started + 1, "{shelf}");
            app.focus = Focus::Composer;
            press(&mut app, KeyCode::Char('x'));
            assert_eq!(app.composer.text(), "drafxt", "{shelf}");
        }

        // The sidebar has no row for an agent's thread, so the highlight stays on `t`.
        let mut agent = related("a", "t", "subagent");
        agent["activityRunStatus"] = json!("running");
        let mut app = closed_shelves_lineage_tui(agent);
        let started = reads(&runtime);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(open_id(&app), Some("a"));
        assert_eq!(reads(&runtime), started + 1);
        assert_eq!(app.focus, Focus::Details);
        screen(&mut app, 200, 50);
        assert_eq!(app.sidebar.selected_thread_id(), Some("t"));
        assert!(!clickable(&app).iter().any(|row| row == "a"));
        assert_eq!(app.composer.text(), "draft");
    }

    #[test]
    fn enter_asks_the_shell_again_before_it_opens_a_related_thread() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = lineage_tui();
        let started = reads(&runtime);
        // ↓ chooses the fork, and the frame says Enter opens it.
        presses(&mut app, KeyCode::Down, 2);
        let status = status_after(&mut app);
        assert!(status.contains("Enter open fork"), "{status}");
        let (x, y) = details_row(&mut app, 200, 50, "↳ Thread f");

        // T3 archives the fork before the next frame. A click on its row does nothing, and
        // Enter opens nothing and says why.
        let mut archived = related("f", "t", "fork");
        archived["archivedAt"] = json!("2026-10-12T00:00:00.000Z");
        app.on_shell_event(thread_updated(2, archived));
        assert!(!mouse(&mut app, CLICK, x, y));
        press(&mut app, KeyCode::Enter);
        assert_eq!((open_id(&app), reads(&runtime)), (Some("t"), started));
        let status = status_after(&mut app);
        assert!(
            status.contains("This related thread is unavailable."),
            "{status}"
        );
        // From that frame on, its row says it doesn't open.
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Down);
        let status = status_after(&mut app);
        assert!(status.contains("Fork unavailable"), "{status}");

        // It comes back, and T3 removes it before the next frame.
        app.on_shell_event(thread_updated(3, related("f", "t", "fork")));
        let status = status_after(&mut app);
        assert!(status.contains("Enter open fork"), "{status}");
        let (x, y) = details_row(&mut app, 200, 50, "↳ Thread f");
        let removed = json!({"kind": "thread.removed", "sequence": 4, "threadId": "f"});
        app.on_shell_event(WatchEvent::Item(removed));
        assert!(!mouse(&mut app, CLICK, x, y));
        press(&mut app, KeyCode::Enter);
        assert_eq!((open_id(&app), reads(&runtime)), (Some("t"), started));
        let status = status_after(&mut app);
        assert!(
            status.contains("This related thread is unavailable."),
            "{status}"
        );
        let text = details_after(&mut app, 200, 50);
        assert!(!text.contains("Thread f"), "{text}");

        // The agent's thread is renamed and waits, which leaves it a thread that opens, so
        // Enter before the next frame opens it.
        presses(&mut app, KeyCode::Down, 2);
        let status = status_after(&mut app);
        assert!(status.contains("Enter open subagent"), "{status}");
        let mut waiting = related("a", "t", "subagent");
        waiting["title"] = json!("Read the parser");
        waiting["activityRunStatus"] = json!("waiting");
        app.on_shell_event(thread_updated(5, waiting));
        press(&mut app, KeyCode::Enter);
        assert_eq!(open_id(&app), Some("a"));
        assert_eq!(reads(&runtime), started + 1);
        assert_eq!(app.focus, Focus::Details);
    }

    /// Presses `code` `times` times.
    fn presses(app: &mut App, code: KeyCode, times: usize) {
        for _ in 0..times {
            press(app, code);
        }
    }

    /// The TUI of `lineage_tui`, with five more forks of `t`, `g0` to `g4`, which come after
    /// the fork `f`, drawn on a screen 200 columns by 24 rows. Lineage's first group shows six
    /// of its seven rows and a Show more row.
    fn long_lineage_tui() -> App {
        let mut app = lineage_tui();
        for i in 0..5_u64 {
            let fork = related(&format!("g{i}"), "t", "fork");
            app.on_shell_event(thread_updated(2 + i, fork));
        }
        cells(&mut app, 200, 24);
        app
    }

    /// A move of the details' text, for a click that comes before the frame after it.
    struct Move {
        name: &'static str,
        /// The screen of the frames before the move.
        screen: (u16, u16),
        /// What comes before the frame that draws the row clicked.
        prepare: fn(&mut App),
        act: fn(&mut App),
        /// The screen of the frame after the move.
        after: (u16, u16),
    }

    /// Scrolls the details a row down with the wheel.
    fn wheel_down(app: &mut App) {
        let area = app.details.drawn.expect("the details").area;
        mouse(app, MouseEventKind::ScrollDown, area.x + 2, area.y + 2);
    }

    /// Turns the details into a dialog on a screen 80 columns by 24 rows.
    fn as_dialog(app: &mut App) {
        app.on_terminal_event(Event::Resize(80, 24));
        cells(app, 80, 24);
    }

    /// Makes `step`'s move on a `long_lineage_tui` and clicks before a frame draws the details
    /// again. The click acts on nothing. The next frame draws another thread's row there, and a
    /// click on it opens that thread.
    ///
    /// The row clicked comes from the layout, not from a count of the rows the details show. A
    /// second TUI makes the same move with a frame after it, and the row is the first one that
    /// had a target before the move and has another thread's row after it. The TUI clicked on
    /// gets no frame between the move and the click.
    fn click_after(runtime: &tokio::runtime::Runtime, step: &Move) {
        let name = step.name;
        let (width, height) = step.screen;
        let (after_width, after_height) = step.after;
        let (x, y, was, id) = {
            let mut framed = long_lineage_tui();
            (step.prepare)(&mut framed);
            cells(&mut framed, width, height);
            let area = framed.details.drawn.expect("the details").area;
            let x = area.x + area.width / 2;
            let before: Vec<_> = (area.y..area.bottom())
                .map(|y| (y, framed.details.target_at(x, y)))
                .collect();
            (step.act)(&mut framed);
            cells(&mut framed, after_width, after_height);
            before
                .into_iter()
                .find_map(|(y, was)| match framed.details.target_at(x, y) {
                    Some(lineage::Target::Thread(id))
                        if was.is_some() && was != Some(lineage::Target::Thread(id.clone())) =>
                    {
                        Some((x, y, was, id))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name}: no row has another thread's row after it"))
        };

        let mut app = long_lineage_tui();
        let started = reads(runtime);
        (step.prepare)(&mut app);
        cells(&mut app, width, height);
        let before = app.details.target_at(x, y);
        assert!(before.is_some(), "{name}");
        assert_eq!(before, was, "{name}");
        (step.act)(&mut app);
        let paging = app.details.paging().clone();
        mouse(&mut app, CLICK, x, y);
        assert_eq!(open_id(&app), Some("t"), "{name}");
        assert_eq!(reads(runtime), started, "{name}");
        assert_eq!(app.details.paging(), &paging, "{name}");
        assert!(app.message.is_none(), "{name}");

        let buffer = cells(&mut app, after_width, after_height);
        let now = app.details.target_at(x, y);
        assert_eq!(now, Some(lineage::Target::Thread(id.clone())), "{name}");
        assert_ne!(now, before, "{name}");
        let area = app.details.drawn.expect("the details").area;
        let row = text_in(&buffer, Rect::new(area.x, y, area.width, 1));
        assert!(row.contains(&format!("Thread {id}")), "{name}: {row}");
        assert!(mouse(&mut app, CLICK, x, y), "{name}");
        assert_eq!(open_id(&app), Some(id.as_str()), "{name}");
        assert_eq!(reads(runtime), started + 1, "{name}");
    }

    #[test]
    fn a_click_before_the_frame_after_the_details_move_acts_on_no_row() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let pane = (200, 24);
        let dialog = (80, 24);
        let moves = [
            Move {
                name: "the wheel",
                screen: pane,
                prepare: |_| {},
                act: wheel_down,
                after: pane,
            },
            Move {
                name: "Page Down",
                screen: pane,
                prepare: |_| {},
                act: |app| press(app, KeyCode::PageDown),
                after: pane,
            },
            Move {
                name: "End",
                screen: pane,
                prepare: |_| {},
                act: |app| press(app, KeyCode::End),
                after: pane,
            },
            Move {
                name: "Home",
                screen: pane,
                prepare: |app| press(app, KeyCode::End),
                act: |app| press(app, KeyCode::Home),
                after: pane,
            },
            // ↓ chooses the row below the last one the pane shows, which scrolls it into view.
            Move {
                name: "↓",
                screen: pane,
                prepare: |app| presses(app, KeyCode::Down, 4),
                act: |app| press(app, KeyCode::Down),
                after: pane,
            },
            // Enter on Show more puts the group's last fork in its place.
            Move {
                name: "Show more",
                screen: pane,
                prepare: |app| presses(app, KeyCode::Down, 7),
                act: |app| press(app, KeyCode::Enter),
                after: pane,
            },
            // Previous agents closes at the end of the text, which scrolls back a row.
            Move {
                name: "hiding Previous agents",
                screen: pane,
                prepare: |app| {
                    presses(app, KeyCode::Down, 9);
                    press(app, KeyCode::Enter);
                    cells(app, 200, 24);
                    press(app, KeyCode::Down);
                    cells(app, 200, 24);
                    press(app, KeyCode::Up);
                },
                act: |app| press(app, KeyCode::Enter),
                after: pane,
            },
            // The taller screen shows all of the text, which starts at the top again.
            Move {
                name: "a resize",
                screen: pane,
                prepare: |app| press(app, KeyCode::End),
                act: |app| {
                    app.on_terminal_event(Event::Resize(200, 30));
                },
                after: (200, 30),
            },
            Move {
                name: "the wheel on the dialog",
                screen: dialog,
                prepare: as_dialog,
                act: wheel_down,
                after: dialog,
            },
            Move {
                name: "End on the dialog",
                screen: dialog,
                prepare: as_dialog,
                act: |app| press(app, KeyCode::End),
                after: dialog,
            },
            Move {
                name: "↓ on the dialog",
                screen: dialog,
                prepare: |app| {
                    as_dialog(app);
                    presses(app, KeyCode::Down, 5);
                },
                act: |app| press(app, KeyCode::Down),
                after: dialog,
            },
        ];
        for step in &moves {
            click_after(&runtime, step);
        }
    }

    /// A key or a turn of the wheel for the details.
    type Input = fn(&mut App);

    /// Scrolls the details a row up with the wheel.
    fn wheel_up(app: &mut App) {
        let area = app.details.drawn.expect("the details").area;
        mouse(app, MouseEventKind::ScrollUp, area.x + 2, area.y + 2);
    }

    #[test]
    fn keys_before_a_frame_scroll_the_details_as_they_would_with_frames_between() {
        let end: Input = |app| press(app, KeyCode::End);
        let home: Input = |app| press(app, KeyCode::Home);
        let up: Input = |app| press(app, KeyCode::Up);
        let k: Input = |app| press(app, KeyCode::Char('k'));
        let down: Input = |app| press(app, KeyCode::Down);
        let page_up: Input = |app| press(app, KeyCode::PageUp);
        let page_down: Input = |app| press(app, KeyCode::PageDown);
        let wheel: Input = wheel_up;
        let runs: Vec<(&str, Vec<Input>)> = vec![
            ("End ↑", vec![end, up]),
            ("End k k", vec![end, k, k]),
            ("End PageUp", vec![end, page_up]),
            ("End and the wheel", vec![end, wheel]),
            ("End Home", vec![end, home]),
            // Pages past the end, and a row back up.
            (
                "PageDown three times and ↑",
                vec![page_down, page_down, page_down, up],
            ),
            // ↓ chooses rows below the last one in view, which scroll into view, and the wheel
            // scrolls up from there.
            (
                "↓ to Previous agents and the wheel",
                [vec![down; 9], vec![wheel]].concat(),
            ),
        ];
        for dialog in [false, true] {
            let (width, height) = if dialog { (80, 24) } else { (200, 24) };
            let start = || {
                let mut app = long_lineage_tui();
                if dialog {
                    as_dialog(&mut app);
                }
                app
            };
            let top = details_after(&mut start(), width, height);
            let mut ended = start();
            press(&mut ended, KeyCode::End);
            let bottom = details_after(&mut ended, width, height);
            assert_ne!(top, bottom, "{dialog}: the text scrolls");
            for (name, inputs) in &runs {
                let mut framed = start();
                for input in inputs {
                    input(&mut framed);
                    cells(&mut framed, width, height);
                }
                let expected = details_after(&mut framed, width, height);
                let mut batched = start();
                for input in inputs {
                    input(&mut batched);
                }
                let text = details_after(&mut batched, width, height);
                assert_eq!(text, expected, "{dialog} {name}");
                if *name == "End Home" {
                    assert_eq!(text, top, "{dialog} {name}");
                } else {
                    assert_ne!(text, bottom, "{dialog} {name}: it scrolled up from the end");
                }
                assert_eq!(open_id(&batched), Some("t"), "{dialog} {name}");
                assert_eq!(batched.composer.text(), "draft", "{dialog} {name}");
            }
        }
    }

    // ---- Lineage beside the linked pull requests ----

    /// `lineage_tui` on an environment that links pull requests to threads, with `t` linked to
    /// #31 and #32 and five more forks of `t`, `g0` to `g4`, so its text scrolls on a screen 24
    /// rows tall.
    fn lineage_pulls_tui() -> App {
        let mut client = Client::offline();
        client.runtime.capabilities = json!({"threadPullRequests": true});
        let mut app = App::new(
            Arc::new(client),
            Theme::new(Depth::TrueColor),
            &Settings::default(),
            mpsc::unbounded_channel().0,
        );
        // A thread with a worktree subscribes to its setup, which here sends nothing.
        app.watch_setup = Box::new(|_: &str| mpsc::unbounded_channel().1);
        let links = vec![linked(31, "Bound titles"), linked(32, "Lineage")];
        let mut app = lineage_on(app, Some(links));
        for i in 0..5_u64 {
            let fork = related(&format!("g{i}"), "t", "fork");
            app.on_shell_event(thread_updated(2 + i, fork));
        }
        app
    }

    /// A pull request's detail as T3 sends it for #31: behind its base by three commits, with
    /// a reviewer, a label, a check that passed and one that runs, and a description.
    fn host_detail() -> pull_request::Detail {
        let detail = json!({
            "provider": "github",
            "capabilities": {"diff": true, "comment": true, "actions": [], "mergeMethods": [],
                "search": true, "review": {"inlineComment": false, "reply": false,
                "resolve": false, "verdicts": []},
                "reviewers": {"request": false, "listCandidates": false}, "labels": true},
            "viewerPermissions": {"actions": [], "comment": false, "resolve": false,
                "verdicts": [], "requestReviewers": false},
            "projectId": "p", "projectTitle": "Demo", "workspaceRoot": "/repo",
            "repository": "krishhgg/t3term", "number": 31, "title": "Bound titles",
            "body": "Adds a **drawer**.\n\nSecond paragraph.",
            "url": "https://github.com/krishhgg/t3term/pull/31", "author": null,
            "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 3,
            "deletions": 1, "changedFiles": 1, "headBranch": "feature-31",
            "baseBranch": "master", "createdAt": "2026-10-13T08:00:00Z",
            "updatedAt": "2026-10-13T11:00:00Z", "mergedAt": null, "closedAt": null,
            "reviewers": [{"login": "octocat", "name": null, "isBot": false}],
            "labels": [{"name": "ui", "color": "ededed"}],
            "checks": [
                {"name": "macOS", "status": "success", "description": null,
                    "url": "https://github.com/krishhgg/t3term/actions/runs/1/job/2"},
                {"name": "Greptile Review", "status": "pending", "description": null,
                    "url": null}
            ],
            "mergeCapabilities": {"merge": true, "squash": true, "rebase": true},
            "baseComparison": "behind", "behindBy": 3
        });
        pull_request::Detail::read(&detail).unwrap()
    }

    /// The conversation on `host_detail`'s pull request: a remark, then octocat's approval
    /// under another casing of their login.
    fn host_activity() -> pull_request::Activity {
        let activity = json!({
            "reviewers": [{"login": "octocat", "name": null, "isBot": false}],
            "comments": [
                {"id": "c1", "kind": "issue-comment", "author": {"login": "mona",
                    "name": null}, "body": "Looks close.<!-- bot: state -->",
                    "createdAt": "2026-10-13T09:00:00Z", "url": null, "path": null,
                    "reviewState": null},
                {"id": "r1", "kind": "review", "author": {"login": "Octocat", "name": null},
                    "body": "", "createdAt": "2026-10-13T10:00:00Z", "url": null,
                    "path": null, "reviewState": "APPROVED"}
            ],
            "commentCount": 2, "commentsTruncated": false, "reviewThreads": [], "commits": []
        });
        pull_request::Activity::read(&activity).unwrap()
    }

    /// Gives `app` the parts of read `ticket`'s answer, in order.
    fn hear(app: &mut App, ticket: u64, parts: Vec<Part>) {
        for part in parts {
            app.on_action_result(ActionResult::PullRequest { ticket, part });
        }
    }

    /// What a read of a pull request named: the project, the host, the repository, the number
    /// and the ticket.
    type Noted = (String, String, String, u64, u64);

    /// Replaces how `app` reads a pull request, so a read only notes what it named.
    fn note_reads(app: &mut App) -> std::rc::Rc<std::cell::RefCell<Vec<Noted>>> {
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let noted = asked.clone();
        app.read_pull = Box::new(move |reference: Reference, ticket| {
            noted.borrow_mut().push((
                reference.project_id().to_string(),
                reference.host().unwrap_or_default().to_string(),
                reference.repository().to_string(),
                reference.number(),
                ticket,
            ));
        });
        asked
    }

    /// `pulls_tui` on an environment that also reads pull requests from their hosts, with
    /// the pane open on the pull requests' tab and the keyboard in it.
    fn readable_pulls_tui(links: Vec<Value>) -> App {
        let capabilities = json!({"threadPullRequests": true, "pullRequests": true});
        let mut app = pulls_tui_on(links, false, capabilities);
        app.focus = Focus::Transcript;
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        cells(&mut app, 200, 50);
        let tab = app.details.drawn.expect("the pane").tabs[1];
        assert!(mouse(&mut app, CLICK, tab.x, tab.y));
        cells(&mut app, 200, 50);
        app
    }

    #[test]
    fn the_arrows_enter_and_a_click_open_a_links_bot_comments_and_the_keys_say_so() {
        let mut app = readable_pulls_tui(vec![linked(31, "Bound titles")]);
        let _asked = note_reads(&mut app);
        press(&mut app, KeyCode::Enter);
        let activity = json!({
            "comments": [
                {"id": "c1", "kind": "issue-comment", "author": {"login": "mona",
                    "name": null}, "body": "Looks close.", "createdAt": "2026-10-13T09:00:00Z",
                    "url": null, "path": null, "reviewState": null},
                {"id": "b1", "kind": "issue-comment", "author": {"login": "ci[bot]",
                    "name": null}, "body": "Beep.", "createdAt": "2026-10-13T10:00:00Z",
                    "url": null, "path": null, "reviewState": null}
            ],
            "commentCount": 2, "commentsTruncated": false, "reviewThreads": [], "commits": []
        });
        let activity = pull_request::Activity::read(&activity).unwrap();
        let parts = vec![
            Part::Detail(Ok(Box::new(host_detail()))),
            Part::Activity(Ok(Box::new(activity))),
            Part::Done,
        ];
        hear(&mut app, 1, parts);
        cells(&mut app, 200, 50);
        let status = |app: &mut App| {
            let buffer = cells(app, 200, 50);
            text_in(&buffer, Rect::new(0, 49, 200, 1))
        };
        assert!(!status(&mut app).contains("Enter "));

        // From the end, ↑ chooses the bots' group, and the keys lead with what Enter does.
        press(&mut app, KeyCode::End);
        cells(&mut app, 200, 50);
        press(&mut app, KeyCode::Up);
        let shown = status(&mut app);
        assert!(shown.contains("Enter show bot comments · "), "{shown}");
        press(&mut app, KeyCode::Enter);
        assert!(app.details.pulls.viewing());
        let shown = status(&mut app);
        assert!(shown.contains("Enter hide bot comments · "), "{shown}");
        let rows = body_after(&mut app, 200, 50);
        let group = rows
            .iter()
            .position(|row| row.trim() == "⌄ 1 bot comment")
            .expect("the open group");
        assert!(
            rows[group..].iter().any(|row| row.trim() == "Beep."),
            "{rows:#?}"
        );

        // A click on the group closes it.
        let body = app.details.drawn.expect("the pane").body;
        let y = body.y + group as u16;
        assert!(mouse(&mut app, CLICK, body.x + 2, y));
        let rows = body_after(&mut app, 200, 50);
        assert_eq!(rows[group].trim(), "› 1 bot comment");
        assert!(!rows.iter().any(|row| row.trim() == "Beep."), "{rows:#?}");
        // ← shows the list, and its keys say nothing of Enter's control.
        press(&mut app, KeyCode::Left);
        assert!(!app.details.pulls.viewing());
        assert!(!status(&mut app).contains("Enter hide"));
    }

    #[test]
    fn opening_a_links_details_reads_its_pull_request_from_its_host() {
        let links = vec![linked(31, "Bound titles"), linked(32, "Lineage")];
        let mut app = readable_pulls_tui(links);
        let asked = note_reads(&mut app);
        assert!(asked.borrow().is_empty());

        // Enter shows #31's details and asks T3 for it through the thread's project, naming
        // the link's host so T3 routes it. The details say the read is under way.
        press(&mut app, KeyCode::Enter);
        let ask = |ticket| {
            let ask = ("p", "github.com", "krishhgg/t3term", 31, ticket);
            (ask.0.into(), ask.1.into(), ask.2.into(), ask.3, ask.4)
        };
        assert_eq!(*asked.borrow(), [ask(1)]);
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Reading from github.com…"), "{text}");

        // The detail shows the Summary tab's sections, and the Comments section waits for the
        // conversation.
        hear(&mut app, 1, vec![Part::Detail(Ok(Box::new(host_detail())))]);
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Reading the conversation…"), "{text}");

        // With the conversation, the sections come before the link's own. The pane is narrow,
        // so its rows are read as one text, each row a word apart.
        let activity = Part::Activity(Ok(Box::new(host_activity())));
        hear(&mut app, 1, vec![activity, Part::Done]);
        let rows = body_after(&mut app, 200, 120);
        let text = rows
            .iter()
            .map(|row| row.trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!text.contains("Reading from"), "{text}");
        assert!(!text.contains("bot: state"), "{text}");
        let order = [
            "#31 Bound titles",
            "Read from github.com just now",
            "This branch is out-of-date with master by 3 commits.",
            "Reviewers",
            "octocat",
            "Approved",
            "Labels",
            "ui",
            "Checks",
            "✓ macOS",
            "○ Greptile Review",
            "Description",
            "Adds a drawer.",
            "Second paragraph.",
            "Comments (2)",
            "Octocat",
            "Approved",
            "mona",
            "Looks close.",
            "URL",
            "Linked by the agent",
        ];
        let mut from = 0;
        for shown in order {
            let at = text[from..].find(shown);
            assert!(at.is_some(), "{shown:?} after byte {from} in {text}");
            from += at.unwrap_or_default() + shown.len();
        }
        for row in &rows {
            if row.contains("✓ macOS") {
                assert!(row.contains("Passed"), "{row}");
            }
            if row.contains("○ Greptile Review") {
                assert!(row.contains("Running"), "{row}");
            }
        }

        // Back to the list and in again within the minute shows that answer without asking.
        // An answer to a read the details no longer wait for changes nothing.
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Enter);
        assert_eq!(asked.borrow().len(), 1);
        hear(&mut app, 7, vec![Part::Detail(Err("stale".into()))]);
        let again = details_after(&mut app, 200, 50);
        assert!(
            again.contains("octocat") && !again.contains("stale"),
            "{again}"
        );

        // A minute on, opening them reads again, and the first read's late answer is dropped.
        app.now += 61_000;
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Enter);
        assert_eq!(asked.borrow().last(), Some(&ask(2)));
        hear(&mut app, 1, vec![Part::Detail(Ok(Box::new(host_detail())))]);
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Reading from github.com…"), "{text}");

        // A failed read says why, and opening the details again asks at once.
        let failure = "Couldn't read this pull request: \u{1b}]52;c;bad\u{7}lost";
        hear(
            &mut app,
            2,
            vec![Part::Detail(Err(failure.into())), Part::Done],
        );
        let rows = body_after(&mut app, 200, 120);
        let text = rows
            .iter()
            .map(|row| row.trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("Couldn't read this pull request: ]52;c;badlost"),
            "{text}"
        );
        assert!(!text.contains("octocat"), "{text}");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Enter);
        assert_eq!(asked.borrow().last(), Some(&ask(3)));

        // Another link opened while #31's read is under way waits for it, and says it reads.
        // Only the last read is kept, so #31's answer is dropped and #32 is asked for once
        // #31's read is done.
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(asked.borrow().last(), Some(&ask(3)));
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Reading from github.com…"), "{text}");
        hear(&mut app, 3, vec![Part::Detail(Ok(Box::new(host_detail())))]);
        assert_eq!(asked.borrow().len(), 3);
        hear(&mut app, 3, vec![Part::Done]);
        let last = asked.borrow().last().cloned().expect("a read");
        assert_eq!((last.3, last.4), (32, 4));
        let text = details_after(&mut app, 200, 50);
        assert!(
            text.contains("Reading from github.com…") && !text.contains("octocat"),
            "{text}"
        );

        // A link opened and left while #32's read is under way is never asked for.
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Left);
        hear(
            &mut app,
            4,
            vec![Part::Detail(Err("late".into())), Part::Done],
        );
        assert_eq!(asked.borrow().len(), 4);
        press(&mut app, KeyCode::Enter);
        assert_eq!(asked.borrow().last(), Some(&ask(5)));

        // A link that waits goes when the pane closes, so #31's answer asks for nothing, and
        // opening the pane on its details again asks for it.
        let numbers = |asked: &std::cell::RefCell<Vec<Noted>>| {
            let asked = asked.borrow();
            asked.iter().map(|ask| (ask.3, ask.4)).collect::<Vec<_>>()
        };
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        alt(&mut app, 'b');
        assert!(!app.details.open);
        let detail = Part::Detail(Ok(Box::new(host_detail())));
        hear(&mut app, 5, vec![detail, Part::Done]);
        assert_eq!(asked.borrow().len(), 5);
        alt(&mut app, 'b');
        assert_eq!(numbers(&asked).last(), Some(&(32, 6)));

        // So does one that waits when the Thread tab shows. A click on the tab's name puts
        // the keyboard back in the pane.
        cells(&mut app, 200, 50);
        let tabs = app.details.drawn.expect("the pane").tabs;
        assert!(mouse(&mut app, CLICK, tabs[1].x, tabs[1].y));
        cells(&mut app, 200, 50);
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert_eq!(asked.borrow().len(), 6);
        cells(&mut app, 200, 50);
        assert!(mouse(&mut app, CLICK, tabs[0].x, tabs[0].y));
        let detail = Part::Detail(Ok(Box::new(host_detail())));
        hear(&mut app, 6, vec![detail, Part::Done]);
        assert_eq!(asked.borrow().len(), 6);
        cells(&mut app, 200, 50);
        assert!(mouse(&mut app, CLICK, tabs[1].x, tabs[1].y));
        assert_eq!(numbers(&asked).last(), Some(&(31, 7)));
    }

    #[test]
    fn a_link_is_read_only_where_the_environment_reads_pull_requests() {
        // Without `pullRequests` in the environment's capabilities, the details show what T3
        // synced and ask for nothing, as the desktop's panel offers no page.
        let capabilities = json!({"threadPullRequests": true});
        let mut app = pulls_tui_on(vec![linked(31, "Bound titles")], false, capabilities);
        let asked = note_reads(&mut app);
        app.focus = Focus::Transcript;
        cells(&mut app, 200, 50);
        alt(&mut app, 'b');
        cells(&mut app, 200, 50);
        let tab = app.details.drawn.expect("the pane").tabs[1];
        assert!(mouse(&mut app, CLICK, tab.x, tab.y));
        cells(&mut app, 200, 50);
        press(&mut app, KeyCode::Enter);
        assert!(app.details.pulls.viewing());
        let text = details_after(&mut app, 200, 50);
        assert!(text.contains("Linked by the agent"), "{text}");
        assert!(!text.contains("Reading from"), "{text}");
        assert!(asked.borrow().is_empty());

        // A click that opens the details reads as Enter does.
        let mut app = readable_pulls_tui(vec![linked(31, "Bound titles")]);
        let asked = note_reads(&mut app);
        let drawn = app.details.drawn.expect("the pane");
        let (x, y) = (drawn.body.x + 1, drawn.body.y);
        assert!(mouse(&mut app, CLICK, x, y));
        assert!(app.details.pulls.viewing());
        assert_eq!(asked.borrow().len(), 1);
    }

    #[test]
    fn lineage_and_the_linked_pull_requests_keep_their_place_on_their_own_tabs() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        // The work a frame or an event can do for the details.
        let counts = |app: &App| {
            let details = &app.details;
            let builds = details.pulls.builds;
            (
                details.prepares,
                details.wraps,
                details.lineage_prepares,
                builds,
            )
        };
        for dialog in [false, true] {
            let (width, height) = if dialog { (80, 24) } else { (200, 24) };
            let esc = if dialog { "Esc close" } else { "Esc leave" };
            let mut app = lineage_pulls_tui();
            if dialog {
                as_dialog(&mut app);
            }
            let started = reads(&runtime);
            let scrolled = app.scroll.rows();

            // ↓ chooses Lineage's rows until one under the first screen of text scrolls into
            // view.
            let top = details_after(&mut app, width, height);
            assert!(top.contains("Lineage · 1 running"), "{dialog}: {top}");
            let mut presses = 0;
            let (id, reading) = loop {
                press(&mut app, KeyCode::Down);
                presses += 1;
                let text = details_after(&mut app, width, height);
                if text != top {
                    let Some(lineage::Target::Thread(id)) = app.details.selected().cloned() else {
                        panic!("{dialog}: no thread chosen after {presses} presses");
                    };
                    break (id, text);
                }
                assert!(presses < 8, "{dialog}: the text never scrolled");
            };
            assert!(
                id == "f" || id.starts_with('g'),
                "{dialog}: {id} is no fork"
            );
            let chosen = Some(lineage::Target::Thread(id.clone()));
            let area = app.details.drawn.expect("the details").area;
            let x = area.x + area.width / 2;
            let y = (area.y..area.bottom())
                .find(|&y| app.details.target_at(x, y) == chosen)
                .expect("the chosen row in view");
            let status = first_status(&mut app, width, height, Some(esc));
            assert!(status.contains("Enter open fork"), "{dialog}: {status}");
            let before = counts(&app);

            // → shows the pull requests. Until a frame draws them, the chosen row's cell takes
            // no click, and the first frame offers the list's keys.
            press(&mut app, KeyCode::Right);
            assert_eq!(app.details.tab(), details::Tab::PullRequests, "{dialog}");
            assert_eq!(app.details.target_at(x, y), None, "{dialog}");
            assert!(!mouse(&mut app, CLICK, x, y), "{dialog}");
            assert_eq!(open_id(&app), Some("t"), "{dialog}");
            let status = first_status(&mut app, width, height, Some(esc));
            assert!(status.contains("← thread"), "{dialog}: {status}");
            let text = details_after(&mut app, width, height);
            for shown in ["#31 Bound titles", "#32 Lineage"] {
                assert!(text.contains(shown), "{dialog}: {shown:?} in {text}");
            }
            assert!(!text.contains(&format!("Thread {id}")), "{dialog}: {text}");
            assert_eq!(app.details.target_at(x, y), None, "{dialog}");

            // ↓ and Enter show #32's details, and Enter acts on no Lineage row.
            press(&mut app, KeyCode::Down);
            press(&mut app, KeyCode::Enter);
            assert!(app.details.pulls.viewing(), "{dialog}");
            assert_eq!(open_id(&app), Some("t"), "{dialog}");
            let status = first_status(&mut app, width, height, Some(esc));
            assert!(status.contains("← list"), "{dialog}: {status}");
            let text = details_after(&mut app, width, height);
            for shown in ["← List", "2 of 2", "#32 Lineage"] {
                assert!(text.contains(shown), "{dialog}: {shown:?} in {text}");
            }

            // ← goes back to the list, and `h` to the Thread tab. Until a frame draws it, a
            // click on the chosen row does nothing. Then the text, the choice and the row's
            // cell are where they were, and the first frame says what Enter does.
            press(&mut app, KeyCode::Left);
            press(&mut app, KeyCode::Char('h'));
            assert_eq!(app.details.tab(), details::Tab::Thread, "{dialog}");
            assert_eq!(app.details.target_at(x, y), None, "{dialog}");
            assert!(!mouse(&mut app, CLICK, x, y), "{dialog}");
            assert_eq!(open_id(&app), Some("t"), "{dialog}");
            let status = first_status(&mut app, width, height, Some(esc));
            assert!(status.contains("Enter open fork"), "{dialog}: {status}");
            assert_eq!(details_after(&mut app, width, height), reading, "{dialog}");
            assert_eq!(app.details.selected(), chosen.as_ref(), "{dialog}");
            assert_eq!(app.details.target_at(x, y), chosen, "{dialog}");

            // The pull requests kept their place too, with #32 selected.
            press(&mut app, KeyCode::Right);
            press(&mut app, KeyCode::Enter);
            let text = details_after(&mut app, width, height);
            assert!(text.contains("2 of 2"), "{dialog}: {text}");
            press(&mut app, KeyCode::Left);
            press(&mut app, KeyCode::Left);
            assert_eq!(details_after(&mut app, width, height), reading, "{dialog}");
            // None of it worked the facts, Lineage or the links out again, wrapped the text,
            // moved the transcript, touched the draft or asked T3 for anything.
            assert_eq!(counts(&app), before, "{dialog}");
            assert_eq!(app.scroll.rows(), scrolled, "{dialog}");
            assert_eq!(app.composer.text(), "draft", "{dialog}");
            assert_eq!(reads(&runtime), started, "{dialog}");

            // Enter on the pane, or a click on the dialog, opens the chosen fork with one
            // watch. The details keep the keyboard and the Thread tab, the sidebar highlights
            // the fork, and the draft keeps its cursor.
            if dialog {
                assert!(mouse(&mut app, CLICK, x, y), "{dialog}");
            } else {
                press(&mut app, KeyCode::Enter);
            }
            assert_eq!(open_id(&app), Some(id.as_str()), "{dialog}");
            assert_eq!(reads(&runtime), started + 1, "{dialog}");
            assert_eq!(app.focus, Focus::Details, "{dialog}");
            assert_eq!(app.details.tab(), details::Tab::Thread, "{dialog}");
            assert_eq!(
                app.sidebar.selected_thread_id(),
                Some(id.as_str()),
                "{dialog}"
            );
            press(&mut app, KeyCode::Esc);
            app.focus = Focus::Composer;
            press(&mut app, KeyCode::Char('x'));
            assert_eq!(app.composer.text(), "drafxt", "{dialog}");
        }
    }

    // ---- long titles ----

    /// How many times a frame shows a project's badge, two ASCII letters, in `color`.
    fn badges_in(buffer: &ratatui::buffer::Buffer, letters: &str, color: Color) -> usize {
        let (first, second) = letters.split_at(1);
        let shows = |x: u16, y: u16, letter: &str| {
            let cell = &buffer[(x, y)];
            cell.symbol() == letter && cell.fg == color
        };
        let area = buffer.area;
        (0..area.height)
            .flat_map(|y| (1..area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| shows(x - 1, y, first) && shows(x, y, second))
            .count()
    }

    /// A shell event that lists project `id` titled `title`.
    fn project_updated(sequence: u64, id: &str, title: &str) -> WatchEvent {
        let project = json!({"id": id, "title": title});
        let item = json!({"kind": "project.updated", "sequence": sequence, "project": project});
        WatchEvent::Item(item)
    }

    #[test]
    fn a_badge_is_worked_out_when_its_project_changes_and_never_for_a_frame() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        // The badge's second letter and its color come from the end of a title no row shows.
        let name = format!("alpha {} zulu", "x".repeat(200_000));
        let colors = Theme::new(Depth::TrueColor).project_colors;
        let color = |title: &str| colors[theme::project_color_index(title)];
        let thread = bound("t", "p", Value::Null, json!("main"));
        let projects = json!([{"id": "p", "title": &name}, {"id": "q", "title": "Other"}]);
        let snapshot = json!({"snapshotSequence": 1, "projects": projects,
            "threads": [thread.clone()]});
        let shell = json!({"kind": "snapshot", "snapshot": snapshot});
        let mut app = tui(&Settings::default());
        app.on_shell_event(WatchEvent::Item(shell));
        app.on_shell_event(caught_up());
        app.open_thread(open_bound(thread.clone()));
        // The snapshot worked out a badge for each of its projects.
        assert_eq!((app.badges.prepares, app.badges.count()), (2, 2));
        let first = app.badges.get("p").clone();
        assert_eq!(first.letters, "AZ");
        // The sidebar's card and the breadcrumb show it in the whole title's color.
        let buffer = cells(&mut app, 120, 36);
        assert_eq!(badges_in(&buffer, "AZ", color(&name)), 2);

        // Frames at every size, with the sidebar shown or hidden, work out no badge.
        for hidden in [false, true] {
            if hidden {
                ctrl_b(&mut app);
            }
            for (width, height) in [(200, 50), (120, 36), (80, 24), (40, 12)] {
                cells(&mut app, width, height);
                cells(&mut app, width, height);
            }
        }
        assert_eq!(app.badges.prepares, 2);
        // Hidden, the sidebar's card is gone and the breadcrumb still shows the badge.
        let buffer = cells(&mut app, 120, 36);
        assert_eq!(badges_in(&buffer, "AZ", color(&name)), 1);
        ctrl_b(&mut app);

        // Nor do an update to a thread, the open thread's output, the feed catching up, the
        // enrichment of a repository or a project update the shell has already had.
        let mut renamed = thread.clone();
        renamed["title"] = json!("Fix the login form");
        app.on_shell_event(thread_updated(2, renamed));
        let answer = json!({"id": "a20", "type": "assistant_message", "ordinal": 20,
            "text": "Done"});
        app.on_thread_event(live(2, "turn-item.updated", answer));
        app.on_shell_event(caught_up());
        let enrichment = json!({"kind": "snapshot", "snapshot": {"projects": []},
            "resolvedRepositoryIdentityRoots": []});
        app.on_shell_event(WatchEvent::Item(enrichment));
        app.on_shell_event(project_updated(2, "p", "Bravo Yankee"));
        cells(&mut app, 120, 36);
        assert_eq!(app.badges.prepares, 2);
        assert_eq!(app.badges.get("p"), &first);

        // A rename works out the project's badge again.
        app.on_shell_event(project_updated(3, "p", "Bravo Yankee"));
        assert_eq!(app.badges.prepares, 3);
        assert_eq!(app.badges.get("p"), &theme::Badge::of("Bravo Yankee"));
        let drawn = chrome(&mut app, 120, 36);
        assert!(
            drawn
                .header
                .contains("BY Bravo Yankee  /  Fix the login form"),
            "{drawn:?}"
        );
        let buffer = cells(&mut app, 120, 36);
        assert_eq!(badges_in(&buffer, "BY", color("Bravo Yankee")), 2);

        // Removing the project drops its badge, and its thread shows as in no project.
        let removed = json!({"kind": "project.removed", "sequence": 4, "projectId": "p"});
        app.on_shell_event(WatchEvent::Item(removed));
        assert_eq!(app.badges.prepares, 3);
        assert!(!app.badges.has("p"));
        assert_eq!(app.badges.count(), 1);
        let drawn = chrome(&mut app, 120, 36);
        assert!(
            drawn.header.contains("PT Project  /  Fix the login form"),
            "{drawn:?}"
        );

        // Restored, the project has its first badge again, in the same color.
        app.on_shell_event(project_updated(5, "p", &name));
        assert_eq!(app.badges.prepares, 4);
        assert_eq!(app.badges.get("p"), &first);
        let buffer = cells(&mut app, 120, 36);
        assert_eq!(badges_in(&buffer, "AZ", color(&name)), 2);

        // A snapshot without project q works out the badges again and keeps none for q.
        let snapshot = json!({"snapshotSequence": 6, "projects": [{"id": "p", "title": &name}],
            "threads": [thread]});
        let shell = json!({"kind": "snapshot", "snapshot": snapshot});
        app.on_shell_event(WatchEvent::Item(shell));
        assert_eq!((app.badges.prepares, app.badges.count()), (5, 1));
        assert!(!app.badges.has("q"));
        assert_eq!(app.badges.get("p"), &first);
        assert_eq!(reads(&runtime), 0);
    }

    /// The TUI connected to Studio Mac, with thread `t` of Demo titled `title` on `branch`
    /// listed and open, the transcript scrolled up and a draft in the composer.
    fn titled(title: &str, branch: &str) -> App {
        let mut thread = bound("t", "p", Value::Null, json!(branch));
        thread["title"] = json!(title);
        let mut app = tui_at(Some("Studio Mac"), "http://127.0.0.1:51024");
        app.on_shell_event(details_shell(vec![thread.clone()]));
        app.on_shell_event(caught_up());
        app.open_thread(open_bound(thread));
        app.focus = Focus::Composer;
        app.composer.insert_str("half a thought");
        app.scroll.set(12);
        app
    }

    /// Where the last frame of `app` put the details' button, the transcript, the composer and
    /// the sidebar's list.
    fn layout(app: &App) -> [Rect; 4] {
        [
            app.details.button,
            app.transcript_area,
            app.composer_area,
            app.sidebar.list,
        ]
    }

    #[test]
    fn a_huge_title_and_branch_move_nothing_else_on_the_screen() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let title = format!(
            "\u{1b}]0;x\u{7}Fix the login page {}\r\nevil",
            "👩\u{200d}💻e\u{301}名".repeat(20_000)
        );
        let branch = format!("\u{1b}[31mfeature/{}\u{1b}[0m\nevil", "名".repeat(100_000));
        let mut plain = titled("Fix the login page", "feature/login");
        let mut huge = titled(&title, &branch);
        let whole = "Studio Mac · 127.0.0.1:51024";
        // Whether a row shows any of the address.
        let address = |row: &str| row.contains(|c: char| c == '·' || c.is_ascii_digit());
        // The breadcrumb up to the slash before the title.
        let lead = |drawn: &Chrome| Some(drawn.header.split_once("/  ")?.0.to_string());
        for hidden in [false, true] {
            if hidden {
                ctrl_b(&mut plain);
                ctrl_b(&mut huge);
            }
            for (width, height) in [(200, 50), (120, 36), (80, 24), (40, 12)] {
                let at = format!("{hidden} {width}x{height}");
                let plain_drawn = chrome(&mut plain, width, height);
                let huge_drawn = chrome(&mut huge, width, height);
                // The environment shows as it does beside the short title. Where a row has no
                // room for the whole address, `environment::chip` shows the label alone or
                // nothing, and never part of the port. The dots, the status line and the
                // sidebar's top rows are as they were.
                let shown = header_identity(&huge_drawn);
                assert_eq!(shown, header_identity(&plain_drawn), "{at}");
                for row in [shown, huge_drawn.environment.trim()] {
                    assert!(row == whole || !address(row), "{at}: {huge_drawn:?}");
                }
                assert_eq!(huge_drawn.sidebar_dot, plain_drawn.sidebar_dot, "{at}");
                assert_eq!(huge_drawn.header_dot, plain_drawn.header_dot, "{at}");
                assert_eq!(huge_drawn.status, plain_drawn.status, "{at}");
                assert_eq!(huge_drawn.wordmark, plain_drawn.wordmark, "{at}");
                assert_eq!(huge_drawn.environment, plain_drawn.environment, "{at}");
                assert_eq!(lead(&huge_drawn), lead(&plain_drawn), "{at}");
                // The details' button, the transcript and the composer are where they were and
                // hold the same cells, and the reader is where they were.
                let plain_cells = cells(&mut plain, width, height);
                let huge_cells = cells(&mut huge, width, height);
                assert_eq!(layout(&huge), layout(&plain), "{at}");
                for area in &layout(&plain)[..3] {
                    for y in area.y..area.bottom() {
                        for x in area.x..area.right() {
                            assert_eq!(huge_cells[(x, y)], plain_cells[(x, y)], "{at} ({x}, {y})");
                        }
                    }
                }
                assert_eq!(
                    (huge.scroll.rows(), &huge.anchor),
                    (plain.scroll.rows(), &plain.anchor),
                    "{at}"
                );
                assert_eq!(huge.focus, Focus::Composer, "{at}");
                assert_eq!(huge.composer.text(), "half a thought", "{at}");
                // No cell holds a control character, and the text after the line break never
                // shows.
                for y in 0..height {
                    for x in 0..width {
                        let symbol = huge_cells[(x, y)].symbol();
                        assert!(
                            !symbol.contains(char::is_control),
                            "{at} ({x}, {y}) {symbol:?}"
                        );
                    }
                }
                let text = text_in(&huge_cells, huge_cells.area);
                assert!(!text.contains("evil"), "{at}: {text}");
                // The title and the branch show by their printable characters.
                if width == 200 {
                    assert!(
                        huge_drawn.header.contains("]0;xFix the login page"),
                        "{at}: {huge_drawn:?}"
                    );
                    let list = text_in(&huge_cells, huge.sidebar.list);
                    assert_eq!(list.contains("[31mfeature/"), !hidden, "{at}: {list}");
                }
            }
        }
        assert_eq!(reads(&runtime), 0);
    }

    #[test]
    fn a_title_keeps_a_spare_column_and_shows_an_ellipsis_without_room() {
        let mut app = tui(&Settings::default());
        let thread = bound("t", "p", Value::Null, Value::Null);
        app.on_shell_event(shell_snapshot(vec![thread.clone()]));
        app.open_thread(open_bound(thread));
        let shown = |width| app.header_line(width, false).to_string();
        // "DO Demo  /  " takes twelve columns, and the title leaves the row's last column.
        assert_eq!(shown(21), "DO Demo  /  Thread t ");
        assert_eq!(shown(20), "DO Demo  /  Thread… ");
        assert_eq!(shown(14), "DO Demo  /  … ");
        // With no room, the `…` alone takes that column, and past the row's end when it must.
        assert_eq!(shown(13), "DO Demo  /  …");
        assert_eq!(shown(5), "DO Demo  /  …");
    }

    // ---- everything at once ----

    /// The TUI with the sidebar listing thread `t` and `t` open, live on Codex in a worktree
    /// of Demo, with everything that can crowd the conversation but a request: a worktree
    /// setup still running, Codex's notice and the thread's error, forty lines each, and
    /// twenty tasks for the running run. A draft waits in the composer.
    fn crowded() -> App {
        let mut app = tui(&Settings::default());
        let subscriptions = Subscriptions::default();
        let opened = subscriptions.clone();
        app.watch_setup = Box::new(move |thread_id: &str| {
            let (sender, events) = mpsc::unbounded_channel();
            opened.lock().unwrap().push((thread_id.to_string(), sender));
            events
        });
        let mut thread = bound("t", "p", json!("/work/repo-t"), json!("t3code/t"));
        thread["providerInstanceId"] = json!("codex");
        thread["activeProviderThreadId"] = json!("pt");
        thread["modelSelection"] = json!({"instanceId": "codex", "model": "gpt-6"});
        app.on_shell_event(details_shell(vec![thread.clone()]));
        let failing = reported("codex", "Codex", "error", "authenticated");
        let failing = saying(failing, &forty_lines("notice"));
        app.on_config_event(config_snapshot(vec![failing], json!({})));
        let steps: Vec<Value> = (1..=20)
            .map(|n| {
                json!({"id": format!("s{n}"), "text": format!("Step {n}"),
                    "status": "pending"})
            })
            .collect();
        let list = json!({"id": "l1", "threadId": "t", "runId": "r1", "nodeId": "n",
            "kind": "todo_list", "status": "active", "steps": steps});
        let more = json!({"thread": thread, "runs": [setup_run("running", true)],
            "plans": [list]});
        app.open_thread(codex_thread("t", Some(&forty_lines("error")), more));
        let stages = setup_stages(["done", "done", "skipped", "running", "pending"]);
        let setup = setup_item("t", "running", 2, stages);
        assert!(deliver(&mut app, &subscriptions, WatchEvent::Item(setup)));
        app.composer.insert_str("half a reply");
        app
    }

    /// Asserts that each area the last frame noted for the mouse and the keys lies on the
    /// screen.
    fn on_screen(app: &App, size: &str) {
        let mut areas = vec![
            app.transcript_area,
            app.panel_area,
            app.composer_area,
            app.picker_area,
            app.sidebar_toggle,
            app.tasks.area,
            app.banner.area,
            app.banner.close,
            app.provider_banner.area,
            app.provider_banner.close,
            app.details.button,
            app.sidebar.list,
            app.sidebar.border,
        ];
        areas.extend(app.chips.iter().map(|&(area, _)| area));
        if let Some(drawn) = app.details.drawn {
            areas.extend([drawn.area, drawn.close]);
        }
        for area in areas {
            assert_eq!(area.intersection(app.screen), area, "{size}: {area:?}");
        }
    }

    /// Draws a frame 80 columns by 24 rows and returns the text of Codex's notice, the
    /// thread's error and the tasks drawer, each empty when the frame drew none.
    fn notices_and_tasks(app: &mut App) -> [String; 3] {
        let buffer = cells(app, 80, 24);
        let areas = [app.provider_banner.area, app.banner.area, app.tasks.area];
        areas.map(|area| text_in(&buffer, area))
    }

    #[test]
    fn a_crowded_thread_stays_on_screen_and_hands_each_key_to_the_right_place() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let request = json!({"id": "q1", "kind": "command", "status": "pending"});
        for (width, height) in [(80, 24), (40, 12)] {
            let mut app = crowded();
            for waiting in [false, true] {
                if waiting {
                    // A command waits for approval, and its panel takes the tasks' place.
                    app.on_thread_event(live(2, "runtime-request.updated", request.clone()));
                }
                let size = format!("{width}x{height} waiting={waiting}");
                cells(&mut app, width, height);
                on_screen(&app, &size);
                if width == 80 {
                    let card = card_lines(&app);
                    assert!(shows(&card, "Setting up worktree"), "{card:#?}");
                    assert!(app.provider_banner.area.height > 0, "{size}");
                    assert!(app.banner.area.height > 0, "{size}");
                    assert_eq!(app.tasks.area.height > 0, !waiting, "{size}");
                    assert_eq!(app.panel_area.height > 0, waiting, "{size}");
                }

                // The model menu opens. The details open as a dialog, which closes the menu
                // and takes the keyboard.
                alt(&mut app, 'm');
                assert!(app.picker.is_some(), "{size}");
                cells(&mut app, width, height);
                assert!(app.picker_area.height > 0, "{size}");
                on_screen(&app, &size);
                alt(&mut app, 'b');
                assert!(app.picker.is_none(), "{size}");
                assert_eq!(app.focus, Focus::Details, "{size}");
                cells(&mut app, width, height);
                let drawn = app.details.drawn.expect("the dialog");
                assert_eq!(drawn.layout, details::Layout::Dialog, "{size}");
                on_screen(&app, &size);

                // Under the dialog, no key reaches the composer, a notice, the tasks, a menu
                // or the request.
                for code in [KeyCode::Char('x'), KeyCode::Enter, KeyCode::Tab] {
                    press(&mut app, code);
                }
                for c in ['o', 'i', 't', 'm', 'a'] {
                    alt(&mut app, c);
                }
                alt_arrow(&mut app, KeyCode::Down);
                assert!(app.details.open && app.focus == Focus::Details, "{size}");
                assert!(!app.provider_banner.expanded(), "{size}");
                assert!(!app.banner.expanded(), "{size}");
                assert!(!app.tasks.open && app.picker.is_none(), "{size}");
                assert_eq!(app.panel_scroll, 0, "{size}");
                assert_eq!(app.composer.text(), "half a reply", "{size}");

                // Esc closes the dialog and gives the keyboard back to the composer, and the
                // menu opens again.
                press(&mut app, KeyCode::Esc);
                assert!(!app.details.open, "{size}");
                assert_eq!(app.focus, Focus::Composer, "{size}");
                alt(&mut app, 'm');
                cells(&mut app, width, height);
                assert!(app.picker.is_some(), "{size}");
                assert!(app.picker_area.height > 0, "{size}");
                on_screen(&app, &size);
                press(&mut app, KeyCode::Esc);
                assert!(app.picker.is_none(), "{size}");
                assert_eq!(app.focus, Focus::Composer, "{size}");
                assert_eq!(app.composer.text(), "half a reply", "{size}");
            }
        }

        // Codex's notice opens, then the tasks under it. Alt+↓ goes to the open notice ahead
        // of the tasks, and opening one notice closes the other.
        let mut app = crowded();
        cells(&mut app, 80, 24);
        alt(&mut app, 'o');
        cells(&mut app, 80, 24);
        alt(&mut app, 't');
        let [notice, _, tasks] = notices_and_tasks(&mut app);
        assert!(notice.contains("Lines 1-"), "{notice}");
        assert!(tasks.contains("Lines 1-8 of 20"), "{tasks}");
        alt_arrow(&mut app, KeyCode::Down);
        let [notice, _, tasks] = notices_and_tasks(&mut app);
        assert!(notice.contains("Lines 2-"), "{notice}");
        assert!(tasks.contains("Lines 1-8 of 20"), "{tasks}");
        // The details open over them as a dialog and take Alt+↓. Esc closes the details
        // first, and the next Alt+↓ is the notice's again.
        alt(&mut app, 'b');
        cells(&mut app, 80, 24);
        alt_arrow(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Esc);
        assert!(!app.details.open);
        assert_eq!(app.focus, Focus::Composer);
        let [notice, _, _] = notices_and_tasks(&mut app);
        assert!(notice.contains("Lines 2-"), "{notice}");
        alt_arrow(&mut app, KeyCode::Down);
        let [notice, _, tasks] = notices_and_tasks(&mut app);
        assert!(notice.contains("Lines 3-"), "{notice}");
        assert!(tasks.contains("Lines 1-8 of 20"), "{tasks}");
        // Alt+I opens the error and closes the notice, and Alt+↓ scrolls the error.
        alt(&mut app, 'i');
        assert!(app.banner.expanded() && !app.provider_banner.expanded());
        notices_and_tasks(&mut app);
        alt_arrow(&mut app, KeyCode::Down);
        let [_, error, tasks] = notices_and_tasks(&mut app);
        assert!(error.contains("Lines 2-"), "{error}");
        assert!(tasks.contains("Lines 1-8 of 20"), "{tasks}");
        // With both closed, Alt+↓ scrolls the tasks.
        alt(&mut app, 'i');
        alt_arrow(&mut app, KeyCode::Down);
        let [_, _, tasks] = notices_and_tasks(&mut app);
        assert!(tasks.contains("Lines 2-9 of 20"), "{tasks}");

        // A command waits for approval. The tasks close and their place goes to its panel,
        // which Alt+↓ now scrolls.
        app.on_thread_event(live(2, "runtime-request.updated", request));
        let [_, _, tasks] = notices_and_tasks(&mut app);
        assert!(tasks.is_empty() && !app.tasks.open);
        assert!(app.panel_area.height > 0);
        let before = app.panel_scroll;
        alt_arrow(&mut app, KeyCode::Down);
        assert_eq!(app.panel_scroll, before + 1);
        // An open notice takes Alt+↓ back from the panel until it closes.
        alt(&mut app, 'o');
        notices_and_tasks(&mut app);
        let before = app.panel_scroll;
        alt_arrow(&mut app, KeyCode::Down);
        assert_eq!(app.panel_scroll, before);
        let [notice, _, _] = notices_and_tasks(&mut app);
        assert!(notice.contains("Lines 2-"), "{notice}");
        alt(&mut app, 'o');
        let before = app.panel_scroll;
        alt_arrow(&mut app, KeyCode::Down);
        assert_eq!(app.panel_scroll, before + 1);
        assert_eq!(app.composer.text(), "half a reply");
        // None of it asked T3 for anything.
        assert_eq!(reads(&runtime), 0);
    }

    // ---- appearance ----

    fn alt_shift_a(app: &mut App) {
        let key = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::ALT | KeyModifiers::SHIFT);
        app.on_terminal_event(Event::Key(key));
    }

    /// Each color `buffer` draws that `theme` doesn't have, with the first cell that draws it.
    fn foreign_colors(buffer: &ratatui::buffer::Buffer, theme: &Theme) -> Vec<(Color, u16, u16)> {
        let palette: HashSet<Color> = theme.colors().into_iter().chain([Color::Reset]).collect();
        let mut seen = HashSet::new();
        let mut foreign = Vec::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                for color in [cell.fg, cell.bg] {
                    if !palette.contains(&color) && seen.insert(color) {
                        foreign.push((color, x, y));
                    }
                }
            }
        }
        foreign
    }

    #[test]
    fn alt_shift_a_redraws_everything_in_the_other_half_at_once() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        // The crowded thread, then with its details beside it, and a linked pull request's
        // details: every part of the screen that keeps its rows from one frame to the next.
        let crowded_details = || {
            let mut app = crowded();
            alt(&mut app, 'b');
            app
        };
        let pull_details = || {
            let mut app = pulls_tui(vec![long_linked(32), long_linked(31)], false);
            app.focus = Focus::Transcript;
            cells(&mut app, 120, 40);
            alt(&mut app, 'b');
            press(&mut app, KeyCode::Right);
            cells(&mut app, 120, 40);
            press(&mut app, KeyCode::Enter);
            app
        };
        let scenes = [
            ("crowded", crowded(), 80, 24),
            ("crowded with details", crowded_details(), 200, 50),
            ("a pull request's details", pull_details(), 120, 40),
        ];
        for (scene, mut app, width, height) in scenes {
            let composer = app.composer.text();
            let dark = cells(&mut app, width, height);
            assert_eq!(app.theme.appearance, Appearance::Dark, "{scene}");
            assert_eq!(foreign_colors(&dark, &app.theme), [], "{scene}");

            // Dark goes to System, which keeps what draws until the system's appearance is
            // read, and the read never answers here. Then Light.
            alt_shift_a(&mut app);
            assert_eq!(app.appearance_mode, AppearanceMode::System, "{scene}");
            assert_eq!(app.theme.appearance, Appearance::Dark, "{scene}");
            alt_shift_a(&mut app);
            assert_eq!(app.appearance_mode, AppearanceMode::Light, "{scene}");
            assert_eq!(app.theme.appearance, Appearance::Light, "{scene}");
            // The next frame draws at once, in the new colors only.
            assert!(app.layout_changed, "{scene}");
            let light = cells(&mut app, width, height);
            assert_eq!(foreign_colors(&light, &app.theme), [], "{scene}");
            let canvas = light.content.iter().filter(|cell| cell.bg == app.theme.bg);
            assert!(canvas.count() > 0, "{scene}");

            // And back to Dark.
            alt_shift_a(&mut app);
            assert_eq!(app.theme.appearance, Appearance::Dark, "{scene}");
            let dark = cells(&mut app, width, height);
            assert_eq!(foreign_colors(&dark, &app.theme), [], "{scene}");
            // No key reached the composer, and each choice was saved.
            assert_eq!(app.composer.text(), composer, "{scene}");
            assert_eq!(
                crate::settings::saved_in_test().appearance_mode,
                AppearanceMode::Dark
            );
        }
        // Only the turn to System read anything.
        assert_eq!(reads(&runtime), 3);
    }

    #[test]
    fn system_draws_the_systems_appearance_read_each_time_it_is_chosen() {
        use appearance::fake::{READS, SYSTEM};
        use theme::Appearance::{Dark, Light};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut app = tui(&Settings::default());
            let (sender, mut results) = mpsc::unbounded_channel();
            app.actions = sender;
            READS.set(0);
            let message = |app: &App| app.message.as_ref().map(|(text, _)| text.clone());

            // Dark to System keeps dark until the read says the system is light.
            SYSTEM.set(Some(Light));
            alt_shift_a(&mut app);
            assert_eq!(message(&app).as_deref(), Some("Appearance: System"));
            assert_eq!(app.theme.appearance, Dark);
            app.on_action_result(results.recv().await.unwrap());
            assert_eq!((app.theme.appearance, READS.get()), (Light, 1));
            assert_eq!(
                crate::settings::saved_in_test().appearance_mode,
                AppearanceMode::System
            );

            // Light and Dark read nothing.
            alt_shift_a(&mut app);
            assert_eq!(message(&app).as_deref(), Some("Appearance: Light"));
            assert_eq!(app.theme.appearance, Light);
            alt_shift_a(&mut app);
            assert_eq!(message(&app).as_deref(), Some("Appearance: Dark"));
            assert_eq!(app.theme.appearance, Dark);
            tokio::task::yield_now().await;
            assert_eq!(READS.get(), 1);

            // Back on System, the last read draws at once, and the system is read again.
            SYSTEM.set(Some(Dark));
            alt_shift_a(&mut app);
            assert_eq!(app.theme.appearance, Light);
            app.on_action_result(results.recv().await.unwrap());
            assert_eq!((app.theme.appearance, READS.get()), (Dark, 2));

            // A read that can't tell sends nothing, and what draws stays.
            SYSTEM.set(None);
            for _ in 0..3 {
                alt_shift_a(&mut app);
            }
            assert_eq!(app.appearance_mode, AppearanceMode::System);
            tokio::task::yield_now().await;
            assert_eq!(READS.get(), 3);
            assert!(results.try_recv().is_err());
            assert_eq!(app.theme.appearance, Dark);

            // A read that answers after the reader moved on is kept for the next turn to
            // System and draws nothing now.
            SYSTEM.set(Some(Light));
            for _ in 0..3 {
                alt_shift_a(&mut app);
            }
            alt_shift_a(&mut app);
            assert_eq!(app.appearance_mode, AppearanceMode::Light);
            app.on_action_result(results.recv().await.unwrap());
            assert_eq!(app.system_appearance, Some(Light));
            alt_shift_a(&mut app);
            alt_shift_a(&mut app);
            assert_eq!(app.appearance_mode, AppearanceMode::System);
            assert_eq!(app.theme.appearance, Light);
            app.on_action_result(results.recv().await.unwrap());
            assert!(results.try_recv().is_err());

            // Two reads that answer out of order: the newer one's answer draws, and the
            // older one's, arriving after it, is dropped.
            for system in [Light, Dark] {
                SYSTEM.set(Some(system));
                for _ in 0..3 {
                    alt_shift_a(&mut app);
                }
                tokio::task::yield_now().await;
            }
            assert_eq!(app.appearance_mode, AppearanceMode::System);
            let older = results.recv().await.unwrap();
            let newer = results.recv().await.unwrap();
            app.on_action_result(newer);
            assert_eq!(app.theme.appearance, Dark);
            app.on_action_result(older);
            assert_eq!(app.system_appearance, Some(Dark));
            assert_eq!(app.theme.appearance, Dark);
        });
    }

    #[test]
    fn a_built_in_theme_draws_both_halves_in_its_own_colors() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        // The crowded thread with its details, a pending approval and a scrolled transcript.
        let crowded_details = || {
            let mut app = crowded();
            let request = json!({"id": "q1", "kind": "command", "status": "pending"});
            app.on_thread_event(live(2, "runtime-request.updated", request));
            app.composer.insert('x');
            alt(&mut app, 'b');
            cells(&mut app, 200, 50);
            press(&mut app, KeyCode::PageUp);
            app
        };
        // A running step, a menu's check and the sidebar's bar, which mark what is current.
        let marks = || {
            let mut app = tui(&Settings::default());
            let thread = bound("t", "p", json!("/work/repo-t"), json!("t3code/t"));
            app.on_shell_event(details_shell(vec![thread.clone()]));
            let steps = json!([{"id": "s1", "text": "Fix the bug", "status": "running"}]);
            let list = json!({"id": "l1", "threadId": "t", "runId": null, "nodeId": "n",
                "kind": "todo_list", "status": "active", "steps": steps});
            let more = json!({"thread": thread, "plans": [list]});
            app.open_thread(codex_thread("t", None, more));
            app.on_config_event(config_snapshot(vec![codex("ready")], json!({})));
            cells(&mut app, 120, 40);
            alt(&mut app, 't');
            app.picker = Some(mode_menu());
            app.focus = Focus::Sidebar;
            app
        };
        // A linked pull request's details, with the way back to the list.
        let pull_details = || {
            let mut app = pulls_tui(vec![long_linked(32), long_linked(31)], false);
            app.focus = Focus::Transcript;
            cells(&mut app, 120, 40);
            alt(&mut app, 'b');
            press(&mut app, KeyCode::Right);
            cells(&mut app, 120, 40);
            press(&mut app, KeyCode::Enter);
            app
        };
        for name in ThemeName::ALL.into_iter().skip(1) {
            // Each scene with its size and the text it draws only in the focus color.
            let pull = "a pull request's details";
            let scenes = [
                ("crowded with details", crowded_details(), 200, 50, &[][..]),
                ("marks", marks(), 120, 40, &["◉", "✓", "▎"][..]),
                (pull, pull_details(), 120, 40, &["← List"][..]),
            ];
            for (scene, mut app, width, height, marks) in scenes {
                let at = format!("{name:?}, {scene}");
                // As the event loop builds it from a file that names the theme.
                app.theme = Theme::of(name, Appearance::Dark, Depth::TrueColor);
                app.restyle();
                let dark = cells(&mut app, width, height);
                drawn_in_its_own_colors(&dark, &app.theme, marks, &at);
                // Alt+Shift+A changes the half and keeps the theme.
                alt_shift_a(&mut app);
                alt_shift_a(&mut app);
                assert_eq!(app.theme.name, name, "{at}");
                assert_eq!(app.theme.appearance, Appearance::Light, "{at}");
                assert_ne!(app.theme.bg, Theme::light(Depth::TrueColor).bg, "{at}");
                let light = cells(&mut app, width, height);
                drawn_in_its_own_colors(&light, &app.theme, marks, &at);
                alt_shift_a(&mut app);
                assert_eq!(app.theme.name, name, "{at}");
            }
        }
    }

    /// Checks that `buffer` draws only `theme`'s colors, on its canvas, with its focus color
    /// drawing each of `marks`, which mark what is current, and its action color filling
    /// buttons and never drawing text.
    fn drawn_in_its_own_colors(
        buffer: &ratatui::buffer::Buffer,
        theme: &Theme,
        marks: &[&str],
        at: &str,
    ) {
        assert_eq!(foreign_colors(buffer, theme), [], "{at}");
        let cells = || buffer.content.iter();
        assert!(cells().any(|cell| cell.bg == theme.bg), "{at}");
        assert!(cells().any(|cell| cell.fg == theme.focus), "{at}");
        // Where each mark starts, as a run of cells that each hold one of its characters.
        let area = buffer.area;
        let starts = |mark: &str| {
            let glyphs: Vec<String> = mark.chars().map(String::from).collect();
            let rows = area.top()..area.bottom();
            let spots = rows.flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)));
            spots
                .filter(|&(x, y)| {
                    let fits = x as usize + glyphs.len() <= area.right() as usize;
                    fits && glyphs
                        .iter()
                        .enumerate()
                        .all(|(at, glyph)| buffer[(x + at as u16, y)].symbol() == glyph)
                })
                .collect::<Vec<_>>()
        };
        for mark in marks {
            let spots = starts(mark);
            assert!(!spots.is_empty(), "{at}: no {mark}");
            for spot in spots {
                assert_eq!(buffer[spot].fg, theme.focus, "{at}: {mark} at {spot:?}");
            }
        }
        if theme.primary != theme.focus {
            assert_eq!(
                cells().filter(|cell| cell.fg == theme.primary).count(),
                0,
                "{at}"
            );
        }
    }

    #[test]
    fn text_on_the_primary_color_takes_its_own_color() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = crowded();
        let request = json!({"id": "q1", "kind": "command", "status": "pending"});
        app.on_thread_event(live(2, "runtime-request.updated", request));
        app.composer.insert('x');
        cells(&mut app, 80, 24);
        press(&mut app, KeyCode::PageUp);
        for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
            while app.appearance_mode != mode {
                alt_shift_a(&mut app);
            }
            let t = app.theme.clone();
            let buffer = cells(&mut app, 80, 24);
            // The send button, Approve and how far the transcript is scrolled, and nothing on
            // the primary color in another.
            let on_primary: String = buffer
                .content
                .iter()
                .filter(|cell| cell.bg == t.primary)
                .inspect(|cell| assert_eq!(cell.fg, t.on_primary, "{mode:?}"))
                .map(|cell| cell.symbol())
                .collect();
            assert!(on_primary.contains("Approve"), "{mode:?}: {on_primary}");
            assert!(on_primary.contains('↑'), "{mode:?}: {on_primary}");
            assert!(on_primary.contains("more"), "{mode:?}: {on_primary}");
        }
    }

    #[test]
    fn alt_a_still_answers_an_approval_and_alt_shift_a_never_does() {
        let runtime = idle_runtime();
        let _entered = runtime.enter();
        let mut app = crowded();
        let request = json!({"id": "q1", "kind": "command", "status": "pending"});
        app.on_thread_event(live(2, "runtime-request.updated", request));
        cells(&mut app, 80, 24);
        alt_shift_a(&mut app);
        assert_eq!(app.appearance_mode, AppearanceMode::System);
        // The turn to System read the system, and nothing answered the request.
        assert_eq!(reads(&runtime), 1);
        alt(&mut app, 'a');
        assert_eq!(app.appearance_mode, AppearanceMode::System);
        // Alt+A sent the answer.
        assert_eq!(reads(&runtime), 2);
    }
}
