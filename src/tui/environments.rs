//! The Environments dialog: the saved environments, and what one check of each one's server
//! found, as `t3term environment status` reports them. Alt+C opens it from any pane, and so
//! does a click on the environment under the sidebar's wordmark.
//!
//! The desktop lists the environments it knows on the Connections page of its settings, each
//! in an `EnvironmentRow` with a dot for its connection's phase
//! (apps/web/src/components/settings/EnvironmentRow.tsx and ConnectionsSettings.tsx at T3
//! f570bd21). t3term holds one connection, to the server it started with. The dialog lists the
//! saved environments and says which of them is that server. It switches to none of them.
//!
//! A check reads `environments.json` and asks each saved environment's server for its
//! descriptor through `Environments::probe_all`: at most 8 at once and 2.5 seconds each, with
//! no login, no `t3`, no Keychain and no WebSocket. It runs when the dialog opens and when `r`
//! asks again, never on a timer. Closing the dialog or asking again stops a check that runs,
//! and the dialog drops what an earlier check sends.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use serde_json::Value;
use tokio::task::AbortHandle;
use unicode_width::UnicodeWidthStr;

use super::environment::Identity;
use super::setup::bounded;
use super::tasks::wrap;
use super::theme::Theme;
use super::{clip, row, with_bg};
use crate::environments::{Environments, Phase, Probe};

/// The widest the dialog draws.
const WIDTH: u16 = 72;
/// The narrowest dialog that gets a border. A narrower one fills its area without one.
const BORDER_MIN_WIDTH: u16 = 12;
/// The most characters of a name kept. A saved name has at most 32, and a file edited by hand
/// can hold a longer one, which the check reports as blocked.
const NAME: usize = 32;
/// The most characters kept of an error's message, a server's version and a T3 home.
const MESSAGE: usize = 512;
const VERSION: usize = 64;
const HOME: usize = 1024;
/// The most rows an error's message wraps to under the list.
const MESSAGE_ROWS: usize = 3;
/// The fewest environments the list keeps on screen before the chosen one's facts give up rows.
const LIST_MIN: usize = 3;
/// Between the parts of a row.
const SEPARATOR: &str = " · ";

/// The status line's keys while the dialog shows.
pub(super) const KEYS: &str = "↑↓ select · r check again · Esc close";

/// Reads the saved environments and checks each one's server, for a dialog in a TUI connected
/// to the environment `current` names.
pub(super) async fn check(current: String) -> Found {
    match crate::environments::load().await {
        Ok(saved) => {
            let probes = saved.probe_all().await;
            Found::of(&saved, probes, &current)
        }
        Err(error) => Found::Unreadable(bounded(&error.to_string(), MESSAGE)),
    }
}

/// What a check found.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Found {
    /// `environments.json` didn't read or parse. The error's message, cleaned.
    Unreadable(String),
    /// Each saved environment in name order. Empty when there is no file, or it saves none.
    Saved(Vec<Saved>),
}

impl Found {
    /// `probes`, from `saved.probe_all()`, for a TUI connected to the environment `current`
    /// names.
    pub(super) fn of(saved: &Environments, probes: Vec<Probe>, current: &str) -> Found {
        Found::Saved(
            probes
                .into_iter()
                .map(|probe| {
                    let home = saved
                        .environments
                        .get(&probe.name)
                        .and_then(|entry| entry.get("t3Home"))
                        .and_then(Value::as_str);
                    Saved::new(probe, home, current)
                })
                .collect(),
        )
    }

    fn saved(&self) -> &[Saved] {
        match self {
            Found::Saved(saved) => saved,
            Found::Unreadable(_) => &[],
        }
    }
}

/// One saved environment as a check found it, cleaned and bounded once.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Saved {
    /// The name without its control characters, at most `NAME` characters.
    name: String,
    /// Whether it is the saved default.
    default: bool,
    /// Whether its server reported the environment id of the server the TUI is connected to.
    current: bool,
    phase: Phase,
    /// The label and the address a ready server reported. Empty when it isn't ready.
    identity: Identity,
    /// The version a ready server reported.
    version: Option<String>,
    /// Why it isn't ready: the error's code and its message, cleaned.
    code: Option<&'static str>,
    message: Option<String>,
    /// The T3 home the file saves for it, when that is text.
    home: Option<String>,
}

impl Saved {
    fn new(probe: Probe, home: Option<&str>, current: &str) -> Saved {
        let ready = probe.phase == Phase::Ready;
        let identity = match (ready, probe.origin.as_deref()) {
            (true, Some(origin)) => Identity::new(probe.label.as_deref(), origin),
            _ => Identity::default(),
        };
        Saved {
            name: bounded(&probe.name, NAME),
            default: probe.default,
            current: ready && probe.environment_id.as_deref() == Some(current),
            phase: probe.phase,
            identity,
            version: probe
                .server_version
                .map(|version| bounded(&version, VERSION))
                .filter(|version| !version.is_empty()),
            code: probe.code,
            message: probe
                .message
                .map(|message| bounded(&message, MESSAGE))
                .filter(|message| !message.is_empty()),
            home: home
                .map(|home| bounded(home, HOME))
                .filter(|home| !home.is_empty()),
        }
    }

    /// The dot's color, as the desktop colors a connection's phase: green when ready, red when
    /// blocked, and muted when offline, as the desktop mutes an environment it can't reach.
    fn color(&self, t: &Theme) -> Color {
        match self.phase {
            Phase::Ready => t.emerald,
            Phase::Offline => t.muted,
            Phase::Blocked => t.error,
        }
    }
}

/// Where the last frame drew the dialog, for the mouse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Drawn {
    /// The whole dialog, its border included.
    pub area: Rect,
    /// The × at the right of its title row.
    pub close: Rect,
    /// The screen row of each environment drawn, with its place in the list.
    pub rows: Vec<(u16, usize)>,
}

/// The dialog, closed until Alt+C or a click opens it.
#[derive(Debug, Default)]
pub(super) struct Dialog {
    open: bool,
    /// What the last check that counted found. None until the first check after opening ends.
    found: Option<Found>,
    /// The number of the last check asked for. A result counts only with this number.
    asked: u64,
    /// Whether that check still runs.
    checking: bool,
    /// Stops that check, when the TUI started a task for it.
    running: Option<AbortHandle>,
    /// The chosen environment's place in the list.
    selected: usize,
    /// The first environment the list shows.
    scroll: usize,
    /// The widest name the last check that counted found, in columns. Measured once when the
    /// check's result comes, so a frame lays out only the rows it shows.
    name_width: usize,
    /// How many environment rows frames have laid out, so a test can count them.
    #[cfg(test)]
    laid_out: usize,
    /// What the last frame drew. None until a frame draws the dialog after a change, and the
    /// mouse acts on none of it until then.
    pub drawn: Option<Drawn>,
}

impl Dialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the dialog with nothing found, and returns the number of the check to start.
    pub fn open(&mut self) -> u64 {
        self.open = true;
        self.found = None;
        self.selected = 0;
        self.scroll = 0;
        self.ask()
    }

    /// Closes the dialog and stops a check that runs. What it found goes with it, so the next
    /// opening shows only what its own check finds.
    pub fn close(&mut self) {
        self.stop();
        self.open = false;
        self.found = None;
        self.drawn = None;
    }

    /// Stops a check that runs, keeps what the last one found, and returns the number of the
    /// check to start in its place.
    pub fn ask(&mut self) -> u64 {
        self.stop();
        self.asked += 1;
        self.checking = true;
        self.drawn = None;
        self.asked
    }

    /// Keeps the handle of the task that runs check `asked`, so closing can stop it. A handle
    /// for a check that no longer counts stops its task at once.
    pub fn started(&mut self, asked: u64, handle: Option<AbortHandle>) {
        match handle {
            Some(handle) if !(self.open && self.checking && asked == self.asked) => handle.abort(),
            handle => self.running = handle,
        }
    }

    fn stop(&mut self) {
        if let Some(handle) = self.running.take() {
            handle.abort();
        }
        self.checking = false;
    }

    /// What check `asked` found. It counts only while the dialog is open and no later check
    /// was asked for, and says whether it counted. The first result after opening chooses the
    /// environment the TUI is connected to, or the first. A later one keeps the chosen
    /// environment when it is still saved.
    pub fn found(&mut self, asked: u64, found: Found) -> bool {
        if !self.open || !self.checking || asked != self.asked {
            return false;
        }
        let kept = self
            .found
            .as_ref()
            .and_then(|old| old.saved().get(self.selected))
            .map(|saved| saved.name.clone());
        let saved = found.saved();
        self.name_width = saved.iter().map(|s| s.name.width()).max().unwrap_or(0);
        self.selected = match kept {
            Some(name) => saved.iter().position(|s| s.name == name),
            None => saved.iter().position(|s| s.current),
        }
        .unwrap_or(0)
        .min(saved.len().saturating_sub(1));
        self.found = Some(found);
        self.running = None;
        self.checking = false;
        self.drawn = None;
        true
    }

    /// Moves the choice `by` environments, stopping at the ends.
    pub fn select_by(&mut self, by: isize) {
        let count = self.count();
        if count == 0 {
            return;
        }
        let next = self.selected.saturating_add_signed(by).min(count - 1);
        self.choose(next);
    }

    /// Chooses the environment at `index`, the last when it is past the end.
    pub fn choose(&mut self, index: usize) {
        let index = index.min(self.count().saturating_sub(1));
        if index != self.selected {
            self.selected = index;
            self.drawn = None;
        }
    }

    pub fn last(&self) -> usize {
        self.count().saturating_sub(1)
    }

    fn count(&self) -> usize {
        self.found.as_ref().map_or(0, |found| found.saved().len())
    }

    /// The screen may have moved, so the mouse acts on nothing until a frame draws again.
    pub fn resized(&mut self) {
        self.drawn = None;
    }

    /// The environment at screen row `y` of the last frame, if it drew one there.
    pub fn row_at(&self, y: u16) -> Option<usize> {
        self.drawn
            .as_ref()?
            .rows
            .iter()
            .find(|(row, _)| *row == y)
            .map(|(_, index)| *index)
    }

    /// Draws the dialog on `screen`, the rows under the title row: a column in from the left,
    /// under the sidebar's environment row it opens from, at most `WIDTH` columns wide with a
    /// column of the screen showing on each side, and as tall as its text down to the row
    /// above the status line. On a screen too small for that it takes what there is.
    /// `connected` is the environment the TUI is connected to.
    pub fn draw(&mut self, frame: &mut Frame, screen: Rect, connected: &Identity, t: &Theme) {
        if screen.width == 0 || screen.height == 0 {
            self.drawn = None;
            return;
        }
        let width = if screen.width >= BORDER_MIN_WIDTH + 2 {
            (screen.width - 2).min(WIDTH)
        } else {
            screen.width
        };
        let margin = u16::from(width < screen.width);
        let (top, bottom) = if screen.height >= 6 { (1, 1) } else { (0, 0) };
        let room = usize::from(screen.height - top - bottom);
        let bordered = width >= BORDER_MIN_WIDTH && room >= 3;
        let frame_rows = if bordered { 2 } else { 0 };
        let text_width = usize::from(if bordered { width - 4 } else { width });

        let head = self.head(text_width, connected, t);
        let stand_in = self.stand_in(text_width, t);
        let listed = stand_in.is_none();
        let count = stand_in.as_ref().map_or_else(|| self.count(), Vec::len);
        let facts = self.facts(text_width, t);
        // The list keeps its first rows before the chosen environment's facts take any, and
        // the facts give up their last rows before the list gives up more.
        let available = room.saturating_sub(frame_rows + head.len());
        let list_min = count.min(LIST_MIN);
        let facts_shown = if facts.is_empty() {
            0
        } else {
            available.saturating_sub(list_min + 1).min(facts.len())
        };
        let list_room = available.saturating_sub(if facts_shown > 0 { facts_shown + 1 } else { 0 });
        let list_shown = count.min(list_room);
        if listed {
            self.scroll = into_view(self.scroll, self.selected, list_shown)
                .min(count.saturating_sub(list_shown));
        } else {
            self.scroll = 0;
        }
        let list = match stand_in {
            Some(lines) => lines.into_iter().take(list_shown).collect(),
            None => self.rows(text_width, list_shown, t),
        };

        let used = head.len() + list_shown + if facts_shown > 0 { facts_shown + 1 } else { 0 };
        let height = (used + frame_rows).min(room);
        let area = Rect {
            x: screen.x + margin,
            y: screen.y + top,
            width,
            height: height as u16,
        };
        let inner = if bordered {
            Rect {
                x: area.x + 2,
                y: area.y + 1,
                width: area.width - 4,
                height: area.height.saturating_sub(2),
            }
        } else {
            area
        };

        let mut lines: Vec<Line<'static>> = Vec::with_capacity(used);
        lines.extend(head);
        let mut rows = Vec::with_capacity(list_shown);
        for (offset, line) in list.into_iter().enumerate() {
            let index = self.scroll + offset;
            let y = inner.y + lines.len() as u16;
            if listed {
                if y < inner.bottom() {
                    rows.push((y, index));
                }
                if index == self.selected {
                    lines.push(Line::from(with_bg(line.spans, t.highlight)));
                    continue;
                }
            }
            lines.push(line);
        }
        if facts_shown > 0 {
            lines.push(Line::default());
            lines.extend(facts.into_iter().take(facts_shown));
        }
        let above = listed && self.scroll > 0;
        let below = listed && self.scroll + list_shown < count;

        frame.render_widget(Clear, area);
        let style = Style::new().fg(t.fg).bg(t.popover);
        if bordered {
            let mut block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(t.border_strong))
                .style(style);
            let more = match (above, below) {
                (true, true) => Some(" more ↑↓ "),
                (true, false) => Some(" more ↑ "),
                (false, true) => Some(" more ↓ "),
                (false, false) => None,
            };
            if let Some(more) = more.filter(|more| more.width() + 2 <= usize::from(area.width)) {
                block = block
                    .title_bottom(Line::styled(more, Style::new().fg(t.muted)).right_aligned());
            }
            frame.render_widget(block, area);
        } else {
            frame.render_widget(Block::new().style(style), area);
        }
        frame.render_widget(Paragraph::new(lines).style(style), inner);
        // The × and the blank column on each side of it.
        let close = if text_width >= 4 && inner.height > 0 {
            Rect::new(inner.right() - 2, inner.y, 3, 1).intersection(area)
        } else {
            Rect::default()
        };
        self.drawn = Some(Drawn { area, close, rows });
    }

    /// The title row, with `checking…` while a check runs and × at its end, then the
    /// environment the TUI is connected to and a blank row.
    fn head(&self, width: usize, connected: &Identity, t: &Theme) -> Vec<Line<'static>> {
        const TITLE: &str = "Environments";
        let muted = Style::new().fg(t.muted);
        let close = if width >= 4 { " ×" } else { "" };
        // The word goes before the title is cut.
        let checking =
            if self.checking && TITLE.width() + 2 + "checking…".width() + close.width() <= width {
                "checking…"
            } else {
                ""
            };
        let title = clip(TITLE, width.saturating_sub(close.width()));
        let mut lines = vec![row(
            vec![Span::styled(
                title,
                Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
            )],
            vec![Span::styled(checking, muted), Span::styled(close, muted)],
            width,
        )];
        const LEAD: &str = "Connected to ";
        let named = connected.spans(width.saturating_sub(LEAD.width()), muted, muted);
        if !named.is_empty() {
            let mut spans = vec![Span::styled(LEAD, muted)];
            spans.extend(named);
            lines.push(Line::from(spans));
        }
        lines.push(Line::default());
        lines
    }

    /// The lines that stand in for the list when it has no environment to show: the check
    /// that hasn't ended, the file that didn't read, or a file that saves none. None when
    /// there are environments.
    fn stand_in(&self, width: usize, t: &Theme) -> Option<Vec<Line<'static>>> {
        let muted = Style::new().fg(t.muted);
        let (text, style) = match &self.found {
            None => {
                let checking = clip("Checking the saved environments…", width);
                return Some(vec![Line::styled(checking, muted)]);
            }
            Some(Found::Unreadable(message)) => (message.as_str(), Style::new().fg(t.error_fg)),
            Some(Found::Saved(saved)) if saved.is_empty() => (
                "No saved environments. `t3term environment add NAME --t3-home PATH` saves one, and r checks again.",
                muted,
            ),
            Some(Found::Saved(_)) => return None,
        };
        let lines = wrap(text, width)
            .iter()
            .map(|piece| Line::styled(clip(piece, width), style))
            .collect();
        Some(lines)
    }

    /// The rows of the `shown` environments from the first the list shows. A frame lays out
    /// these and no others, however many are saved.
    fn rows(&mut self, width: usize, shown: usize, t: &Theme) -> Vec<Line<'static>> {
        let name_width = self.name_width.min(width / 3);
        let saved = self.found.as_ref().map_or(&[][..], Found::saved);
        let rows: Vec<Line<'static>> = saved
            .iter()
            .skip(self.scroll)
            .take(shown)
            .map(|saved| environment_row(saved, name_width, width, t))
            .collect();
        #[cfg(test)]
        {
            self.laid_out += rows.len();
        }
        rows
    }

    /// The chosen environment's facts, for under the list: for a ready one its label and
    /// address, the server's version and its T3 home, and for another why it isn't ready and
    /// its T3 home.
    fn facts(&self, width: usize, t: &Theme) -> Vec<Line<'static>> {
        let Some(saved) = self
            .found
            .as_ref()
            .and_then(|found| found.saved().get(self.selected))
        else {
            return Vec::new();
        };
        let muted = Style::new().fg(t.muted);
        let fg = Style::new().fg(t.fg);
        let mut lines = Vec::new();
        match saved.phase {
            Phase::Ready => {
                let named = saved.identity.spans(width, fg, muted);
                if !named.is_empty() {
                    lines.push(Line::from(named));
                }
                if let Some(version) = &saved.version {
                    lines.push(fact("T3 ", version, width, muted, fg));
                }
            }
            Phase::Offline | Phase::Blocked => {
                if let Some(code) = saved.code {
                    let style = Style::new().fg(saved.color(t));
                    lines.push(Line::styled(clip(code, width), style));
                }
                if let Some(message) = &saved.message {
                    let mut pieces = wrap(message, width);
                    if pieces.len() > MESSAGE_ROWS {
                        pieces.truncate(MESSAGE_ROWS);
                        if let Some(last) = pieces.last_mut() {
                            last.push('…');
                        }
                    }
                    lines.extend(
                        pieces
                            .iter()
                            .map(|piece| Line::styled(clip(piece, width), fg)),
                    );
                }
            }
        }
        if let Some(home) = &saved.home {
            lines.push(fact("T3 home ", home, width, muted, fg));
        }
        lines
    }
}

/// `label` in `muted` and then `value`, cut to `width` columns.
fn fact(label: &'static str, value: &str, width: usize, muted: Style, fg: Style) -> Line<'static> {
    let room = width.saturating_sub(label.width());
    Line::from(vec![
        Span::styled(clip(label, width), muted),
        Span::styled(clip(value, room), fg),
    ])
}

/// One environment's row in `width` columns: its dot, its name padded to `name_width`, its
/// phase, then whether the TUI is connected to it and whether it is the default, and for a
/// ready one as much of its label and address as fits.
fn environment_row(saved: &Saved, name_width: usize, width: usize, t: &Theme) -> Line<'static> {
    let muted = Style::new().fg(t.muted);
    let color = Style::new().fg(saved.color(t));
    let name = clip(&saved.name, name_width);
    let pad = name_width.saturating_sub(name.width());
    let mut spans = vec![
        Span::styled("● ", color),
        Span::styled(name, Style::new().fg(t.fg)),
        Span::raw(" ".repeat(pad + 2)),
        Span::styled(format!("{:<7}", saved.phase.as_str()), color),
    ];
    let mut used: usize = spans.iter().map(|span| span.content.width()).sum();
    let mut gap = "  ";
    let tags = [
        (saved.current, "connected", Style::new().fg(t.primary)),
        (saved.default, "default", muted),
    ];
    for (shown, word, style) in tags {
        if shown && used + gap.width() + word.width() <= width {
            spans.push(Span::styled(gap, muted));
            spans.push(Span::styled(word, style));
            used += gap.width() + word.width();
            gap = SEPARATOR;
        }
    }
    if let Some(room) = width
        .checked_sub(used + gap.width())
        .filter(|&room| room > 0)
    {
        let named = saved.identity.spans(room, muted, muted);
        if !named.is_empty() {
            spans.push(Span::styled(gap, muted));
            spans.extend(named);
        }
    }
    let line = Line::from(spans);
    // A row narrower than its own dot and phase is cut where the screen ends.
    if line.width() > width {
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        return Line::styled(clip(&text, width), Style::new().fg(t.fg));
    }
    row(line.spans, Vec::new(), width)
}

/// The first row to show so row `line` is in view, given the first shown now and how many
/// show.
fn into_view(top: usize, line: usize, visible: usize) -> usize {
    if line < top {
        line
    } else if visible > 0 && line >= top + visible {
        line + 1 - visible
    } else {
        top
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use serde_json::{Map, json};

    use super::*;
    use crate::tui::theme::Depth;

    const CURRENT: &str = "env-studio";

    fn probe(name: &str, phase: Phase) -> Probe {
        let ready = phase == Phase::Ready;
        Probe {
            name: name.into(),
            default: false,
            phase,
            exit_code: match phase {
                Phase::Ready => 0,
                Phase::Offline => 5,
                Phase::Blocked => 4,
            },
            code: match phase {
                Phase::Ready => None,
                Phase::Offline => Some("T3_SERVER_UNAVAILABLE"),
                Phase::Blocked => Some("T3_ENVIRONMENT_UNVERIFIED"),
            },
            message: (!ready).then(|| format!("{name} didn't answer.")),
            environment_id: ready.then(|| format!("env-{name}")),
            label: ready.then(|| format!("{name} Mac")),
            origin: ready.then(|| "http://127.0.0.1:4000".to_string()),
            server_version: ready.then(|| "0.0.46-nightly".to_string()),
        }
    }

    /// The file's environments, each saved with the home `/homes/NAME`.
    fn saved(names: &[&str]) -> Environments {
        let mut environments = BTreeMap::new();
        for name in names {
            let mut entry = Map::new();
            entry.insert("kind".into(), json!("local"));
            entry.insert("t3Home".into(), json!(format!("/homes/{name}")));
            environments.insert(name.to_string(), entry);
        }
        Environments {
            default: None,
            environments,
            other: Map::new(),
        }
    }

    /// A dialog that has opened and has what `probes` found.
    fn dialog(probes: Vec<Probe>) -> Dialog {
        let names: Vec<&str> = probes.iter().map(|probe| probe.name.as_str()).collect();
        let found = Found::of(&saved(&names), probes.clone(), CURRENT);
        let mut dialog = Dialog::default();
        let asked = dialog.open();
        assert!(dialog.found(asked, found));
        dialog
    }

    fn many(count: usize) -> Vec<Probe> {
        (0..count)
            .map(|n| {
                let phase = [Phase::Ready, Phase::Offline, Phase::Blocked][n % 3];
                probe(&format!("env{n:02}"), phase)
            })
            .collect()
    }

    fn draw(dialog: &mut Dialog, width: u16, height: u16) -> Buffer {
        let theme = Theme::new(Depth::TrueColor);
        let connected = Identity::new(Some("Studio Mac"), "http://127.0.0.1:3773");
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let frame = terminal
            .draw(|frame| dialog.draw(frame, frame.area(), &connected, &theme))
            .unwrap();
        frame.buffer.clone()
    }

    fn rows(buffer: &Buffer) -> Vec<String> {
        let area = buffer.area;
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_check_marks_the_connected_server_and_cleans_what_it_shows() {
        let mut current = probe("studio", Phase::Ready);
        current.environment_id = Some(CURRENT.into());
        current.default = true;
        // Another environment whose server reports the same id is the same server.
        let mut twin = probe("twin", Phase::Ready);
        twin.environment_id = Some(CURRENT.into());
        // A failed check reports no id, so nothing it says can make it the connected one.
        let mut hostile = probe("bad\u{1b}[2Jname\u{9b}31m", Phase::Blocked);
        hostile.message = Some(format!(
            "line one\r\n\u{1b}]52;c;aGk=\u{7}{}",
            "x".repeat(4000)
        ));
        let mut labelled = probe("lab", Phase::Ready);
        labelled.label = Some("Lab\u{1b}[31m box\n".into());
        labelled.server_version = Some(format!("\u{7}{}", "9".repeat(500)));
        let offline = probe("laptop", Phase::Offline);
        let probes = vec![current, twin, hostile.clone(), labelled, offline];

        let mut file = saved(&["studio", "twin", "lab", "laptop"]);
        // A home that isn't text shows no home.
        file.environments.insert(
            hostile.name.clone(),
            Map::from_iter([("t3Home".into(), json!(7))]),
        );
        file.environments
            .get_mut("lab")
            .unwrap()
            .insert("t3Home".into(), json!("/homes/\u{1b}[2Jlab"));
        let Found::Saved(found) = Found::of(&file, probes, CURRENT) else {
            panic!("a list");
        };
        let names: Vec<&str> = found.iter().map(|saved| saved.name.as_str()).collect();
        assert_eq!(names, ["studio", "twin", "bad[2Jname31m", "lab", "laptop"]);
        let current: Vec<bool> = found.iter().map(|saved| saved.current).collect();
        assert_eq!(current, [true, true, false, false, false]);
        assert!(found[0].default);
        assert!(found[1..].iter().all(|saved| !saved.default));

        let lab = &found[3];
        let theme = Theme::new(Depth::TrueColor);
        let shown: String = lab
            .identity
            .spans(80, Style::new(), Style::new())
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert_eq!(shown, "Lab[31m box · 127.0.0.1:4000");
        let version = lab.version.as_deref().unwrap();
        assert!(version.chars().count() <= VERSION + 1 && version.ends_with('…'));
        assert_eq!(lab.home.as_deref(), Some("/homes/[2Jlab"));
        assert_eq!(lab.color(&theme), theme.emerald);

        let bad = &found[2];
        assert_eq!(bad.code, Some("T3_ENVIRONMENT_UNVERIFIED"));
        let message = bad.message.as_deref().unwrap();
        assert!(message.starts_with("line one ]52;c;aGk=xxx"), "{message}");
        assert!(message.chars().count() <= MESSAGE + 1, "{}", message.len());
        assert_eq!(bad.home, None);
        assert_eq!(bad.identity, Identity::default());
        assert_eq!(bad.color(&theme), theme.error);
        assert_eq!(found[4].color(&theme), theme.muted);
        for saved in &found {
            let texts = [
                Some(saved.name.as_str()),
                saved.version.as_deref(),
                saved.message.as_deref(),
                saved.home.as_deref(),
            ];
            for text in texts.into_iter().flatten() {
                assert!(!text.contains(char::is_control), "{text:?}");
            }
        }
    }

    #[tokio::test]
    async fn only_the_last_check_counts_and_closing_stops_it() {
        let mut dialog = Dialog::default();
        assert!(!dialog.is_open());
        let first = dialog.open();
        let task = tokio::spawn(std::future::pending::<()>());
        dialog.started(first, Some(task.abort_handle()));
        assert!(dialog.checking);

        // Asking again stops the first check, and what it sends no longer counts.
        let second = dialog.ask();
        assert_ne!(first, second);
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!dialog.found(first, Found::Saved(Vec::new())));
        assert_eq!(dialog.found, None);
        // A handle that comes for a check no longer asked for stops at once.
        let late = tokio::spawn(std::future::pending::<()>());
        dialog.started(first, Some(late.abort_handle()));
        assert!(late.await.unwrap_err().is_cancelled());

        assert!(dialog.found(second, Found::Saved(Vec::new())));
        assert!(!dialog.checking);
        // A check counts once.
        assert!(!dialog.found(second, Found::Unreadable("no".into())));
        assert_eq!(dialog.found, Some(Found::Saved(Vec::new())));

        // Closing stops a check that runs and forgets what was found.
        let third = dialog.ask();
        let running = tokio::spawn(std::future::pending::<()>());
        dialog.started(third, Some(running.abort_handle()));
        dialog.close();
        assert!(running.await.unwrap_err().is_cancelled());
        assert!(!dialog.is_open());
        assert_eq!(dialog.found, None);
        assert!(!dialog.found(third, Found::Saved(Vec::new())));
        // So does a handle that comes after the dialog closed.
        let after = tokio::spawn(std::future::pending::<()>());
        dialog.started(third, Some(after.abort_handle()));
        assert!(after.await.unwrap_err().is_cancelled());

        // Opening again starts from nothing, with a check of its own.
        let fourth = dialog.open();
        assert!(fourth > third && dialog.checking && dialog.found.is_none());
    }

    #[test]
    fn the_choice_starts_on_the_connected_server_and_follows_its_name() {
        let mut current = probe("studio", Phase::Ready);
        current.environment_id = Some(CURRENT.into());
        let probes = vec![
            probe("alpha", Phase::Offline),
            probe("beta", Phase::Ready),
            current,
        ];
        let mut dialog = dialog(probes.clone());
        assert_eq!(dialog.selected, 2);
        dialog.select_by(1);
        assert_eq!(dialog.selected, 2, "the choice stops at the end");
        dialog.select_by(-5);
        assert_eq!(dialog.selected, 0, "and at the start");
        dialog.choose(1);

        // Another check keeps beta chosen where it now is.
        let asked = dialog.ask();
        let mut moved = vec![probe("aaa", Phase::Ready)];
        moved.extend(probes.iter().cloned());
        let names: Vec<&str> = moved.iter().map(|p| p.name.as_str()).collect();
        let found = Found::of(&saved(&names), moved.clone(), CURRENT);
        assert!(dialog.found(asked, found));
        assert_eq!(dialog.selected, 2);

        // When it is gone, the choice goes to the first.
        let asked = dialog.ask();
        let fewer = vec![probe("alpha", Phase::Offline)];
        assert!(dialog.found(asked, Found::of(&saved(&["alpha"]), fewer, CURRENT)));
        assert_eq!(dialog.selected, 0);
        // No environments leave nothing to choose.
        let asked = dialog.ask();
        assert!(dialog.found(asked, Found::Saved(Vec::new())));
        dialog.select_by(1);
        dialog.choose(3);
        assert_eq!((dialog.selected, dialog.last()), (0, 0));
    }

    #[test]
    fn each_state_says_what_the_dialog_knows() {
        let mut dialog = Dialog::default();
        let asked = dialog.open();
        let text = rows(&draw(&mut dialog, 80, 24)).join("\n");
        assert!(text.contains("Environments"), "{text}");
        assert!(text.contains("checking…"), "{text}");
        assert!(
            text.contains("Connected to Studio Mac · 127.0.0.1:3773"),
            "{text}"
        );
        assert!(text.contains("Checking the saved environments…"), "{text}");
        assert!(
            dialog
                .drawn
                .as_ref()
                .is_some_and(|drawn| drawn.rows.is_empty())
        );

        let message = "Could not read /home/.config/t3term/environments.json: expected value";
        assert!(dialog.found(asked, Found::Unreadable(message.into())));
        let text = rows(&draw(&mut dialog, 80, 24)).join("\n");
        assert!(!text.contains("checking…"), "{text}");
        assert!(text.contains("Could not read"), "{text}");
        assert!(text.contains("environments.json: expected"), "{text}");

        let asked = dialog.ask();
        assert!(dialog.found(asked, Found::Saved(Vec::new())));
        let text = rows(&draw(&mut dialog, 80, 24)).join("\n");
        assert!(text.contains("No saved environments."), "{text}");
        assert!(text.contains("r checks again"), "{text}");

        let mut current = probe("studio", Phase::Ready);
        current.environment_id = Some(CURRENT.into());
        current.default = true;
        let mut dialog = self::dialog(vec![
            probe("lab", Phase::Ready),
            probe("laptop", Phase::Offline),
            probe("old", Phase::Blocked),
            current,
        ]);
        let screen = rows(&draw(&mut dialog, 80, 24));
        let text = screen.join("\n");
        let row = |name: &str| {
            screen
                .iter()
                .find(|row| row.contains(&format!("● {name} ")))
                .unwrap_or_else(|| panic!("no row for {name} in {text}"))
                .trim_end()
                .trim_end_matches('│')
                .trim_end()
                .to_string()
        };
        assert!(
            row("lab").ends_with("ready    lab Mac · 127.0.0.1:4000"),
            "{text}"
        );
        assert!(row("laptop").ends_with("offline"), "{text}");
        assert!(row("old").ends_with("blocked"), "{text}");
        assert!(
            row("studio").ends_with("ready    connected · default · studio Mac · 127.0.0.1:4000"),
            "{text}"
        );
        // The connected environment is chosen, so its facts follow the list.
        assert!(text.contains("T3 0.0.46-nightly"), "{text}");
        assert!(text.contains("T3 home /homes/studio"), "{text}");

        // A failed one's facts say why and where its home is.
        dialog.choose(1);
        let text = rows(&draw(&mut dialog, 80, 24)).join("\n");
        assert!(text.contains("T3_SERVER_UNAVAILABLE"), "{text}");
        assert!(text.contains("laptop didn't answer."), "{text}");
        assert!(text.contains("T3 home /homes/laptop"), "{text}");
    }

    #[test]
    fn the_dialog_stays_on_the_screen_and_keeps_the_choice_in_view() {
        let mut hostile = probe(&format!("{}\u{1b}[2J", "n".repeat(60)), Phase::Blocked);
        hostile.message = Some(format!("\u{1b}]52;c;aGk=\u{7}{}", "word ".repeat(400)));
        let mut probes = many(20);
        probes.push(hostile);
        let theme = Theme::new(Depth::TrueColor);
        for (width, height) in [
            (200, 50),
            (120, 36),
            (80, 24),
            (40, 12),
            (20, 6),
            (13, 5),
            (11, 3),
            (6, 2),
            (1, 1),
        ] {
            for chosen in [0, 7, 20] {
                let mut dialog = dialog(probes.clone());
                dialog.choose(chosen);
                let buffer = draw(&mut dialog, width, height);
                let screen = Rect::new(0, 0, width, height);
                let drawn = dialog.drawn.clone().expect("drawn");
                assert_eq!(
                    drawn.area.intersection(screen),
                    drawn.area,
                    "{width}x{height}"
                );
                assert!(drawn.area.width <= WIDTH, "{width}x{height}");
                // Nothing outside the dialog changes.
                for y in 0..height {
                    for x in 0..width {
                        let cell = &buffer[(x, y)];
                        let inside = drawn.area.contains((x, y).into());
                        assert!(
                            inside || (cell.symbol() == " " && cell.bg != theme.popover),
                            "{width}x{height} ({x},{y}) {:?}",
                            cell.symbol()
                        );
                        assert!(!cell.symbol().contains(char::is_control));
                    }
                }
                // Each row the mouse can choose is the environment drawn there, and the chosen
                // one is among them whenever any shows.
                let text = rows(&buffer);
                for &(y, index) in &drawn.rows {
                    assert!(drawn.area.contains((drawn.area.x + 1, y).into()));
                    // A narrow row cuts the name to its first two characters and `…`.
                    let name = &dialog.found.as_ref().unwrap().saved()[index].name;
                    let cut: String = name.chars().take(2).collect();
                    assert!(
                        text[y as usize].contains(&cut),
                        "{width}x{height} {y}: {text:#?}"
                    );
                }
                if !drawn.rows.is_empty() {
                    assert!(
                        drawn.rows.iter().any(|&(_, index)| index == chosen),
                        "{width}x{height} chose {chosen}: {text:#?}"
                    );
                }
            }
        }

        // At 80x24 the 21 environments scroll, and the border says which way there are more.
        let mut dialog = dialog(probes);
        let text = rows(&draw(&mut dialog, 80, 24)).join("\n");
        assert!(text.contains(" more ↓ "), "{text}");
        dialog.choose(20);
        let text = rows(&draw(&mut dialog, 80, 24)).join("\n");
        assert!(text.contains(" more ↑ "), "{text}");
        // Choosing one further up scrolls the list back to it.
        dialog.choose(5);
        let text = rows(&draw(&mut dialog, 80, 24)).join("\n");
        assert!(text.contains(" more ↑↓ "), "{text}");
        // The long name is cut to a third of the row, and the message to three rows.
        dialog.choose(20);
        let text = rows(&draw(&mut dialog, 80, 24));
        assert!(text.iter().any(|row| row.contains("nnn…")), "{text:#?}");
        let words = text.iter().filter(|row| row.contains("word word")).count();
        assert_eq!(words, MESSAGE_ROWS, "{text:#?}");
    }

    #[test]
    fn a_frame_lays_out_only_the_rows_it_shows() {
        let mut dialog = dialog(many(10_000));
        for (width, height) in [(200, 50), (120, 36), (80, 24), (40, 12), (12, 5)] {
            let before = dialog.laid_out;
            let buffer = draw(&mut dialog, width, height);
            let shown = dialog.drawn.as_ref().unwrap().rows.len();
            assert!(shown > 0, "{width}x{height}: {:#?}", rows(&buffer));
            assert_eq!(dialog.laid_out - before, shown, "{width}x{height}");
        }
        // At the end of the list too, and the names keep the width the whole list measured.
        dialog.choose(9_999);
        let before = dialog.laid_out;
        let buffer = draw(&mut dialog, 120, 36);
        let drawn = dialog.drawn.as_ref().unwrap();
        assert_eq!(dialog.laid_out - before, drawn.rows.len());
        assert_eq!(drawn.rows.last().map(|(_, index)| *index), Some(9_999));
        let last = rows(&buffer)
            .into_iter()
            .find(|row| row.contains("env9999"))
            .unwrap();
        assert!(last.contains("env9999  ready"), "{last}");
    }

    #[test]
    fn a_click_finds_only_what_the_last_frame_drew() {
        let mut dialog = dialog(many(4));
        assert_eq!(dialog.row_at(4), None, "nothing drawn yet");
        let buffer = draw(&mut dialog, 80, 24);
        let drawn = dialog.drawn.clone().unwrap();
        let text = rows(&buffer);
        let (y, index) = drawn.rows[2];
        assert_eq!(dialog.row_at(y), Some(index));
        assert!(text[y as usize].contains("env02"));
        assert_eq!(
            text[drawn.close.y as usize].get(..),
            text[drawn.close.y as usize].get(..)
        );
        let close: String = (drawn.close.x..drawn.close.right())
            .map(|x| buffer[(x, drawn.close.y)].symbol())
            .collect();
        assert_eq!(close, " × ");
        // Choosing another redraws the facts, so until the next frame the mouse finds nothing.
        dialog.choose(index);
        assert_eq!(dialog.row_at(y), None);
        draw(&mut dialog, 80, 24);
        assert_eq!(dialog.row_at(y), Some(index));
        dialog.resized();
        assert_eq!(dialog.row_at(y), None);
    }
}
