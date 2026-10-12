//! The worktree setup card: what the open thread's worktree setup is doing, from T3's
//! `subscribeWorktreeSetup` stream, drawn under the thread's first prompt as the nightly desktop
//! draws its `WorktreeSetupCard` (apps/web/src/components/chat/WorktreeSetupCard.tsx). The card
//! only reads. It has none of the desktop's Cancel, Work locally or Open terminal buttons.
//!
//! The open thread holds one subscription and at most one setup, the latest the stream sent for
//! that thread. T3 keeps setups in memory only and sends each one whole, so a new snapshot
//! replaces the held one, nothing is merged, and nothing is kept for another thread or for
//! later.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use super::theme::{Theme, duration_label, parse_iso_ms};
use super::{clip, hash, row, spinner_frame, tasks};
use crate::client::WatchEvent;
use crate::transcript::{clusters, without_controls};

/// The stages T3 reports, in its order, with the desktop's labels: `WORKTREE_SETUP_STAGE_ORDER`
/// and `worktreeSetupStageLabel` in the nightly's packages/contracts/src/worktreeSetup.ts.
const STAGES: [(&str, &str); 5] = [
    ("fetch", "Fetch base branch"),
    ("checkout", "Check out files"),
    ("submodules", "Init submodules"),
    ("setup-script", "Run setup script"),
    ("agent", "Start agent"),
];
const CHECKOUT: usize = 1;
const SCRIPT: usize = 3;
const AGENT: usize = 4;

/// The contract's limits, in characters: `WORKTREE_SETUP_DETAIL_MAX_LENGTH`,
/// `WORKTREE_SETUP_TAIL_LINE_MAX_LENGTH` and `WORKTREE_SETUP_ERROR_MAX_LENGTH`.
const DETAIL: usize = 200;
const TAIL_LINE: usize = 400;
const ERROR: usize = 1000;
/// The contract sets no limit on a branch, a base, a path or the script's name and command. A
/// thousand characters is more than a row shows.
const NAME: usize = 1000;
/// The script output lines T3 keeps, `TAIL_LINE_LIMIT` in apps/server/src/project/
/// WorktreeSetupTracker.ts, and the rows the desktop's output box always has.
const TAIL_ROWS: usize = 4;
/// The rows an error takes at most.
const ERROR_ROWS: usize = 6;
/// The stages read from one snapshot at most. T3 sends five.
const MAX_STAGES: usize = 16;
/// The longest time shown, a hundred days, so a wrong clock can't print a silly number.
const MAX_ELAPSED: i64 = 100 * 24 * 3600 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Phase {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pending,
    Running,
    Done,
    Skipped,
    Warning,
    Failed,
}

/// When a setup or a stage ended: not yet, at a time, or at a time that couldn't be read, which
/// leaves it without an elapsed time as the desktop's `Date.parse` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Open,
    At(i64),
    Unreadable,
}

#[derive(Debug, Clone, PartialEq)]
struct Stage {
    /// Its place in `STAGES`.
    index: usize,
    status: Status,
    started: Option<i64>,
    ended: End,
    percent: Option<u8>,
    detail: Option<String>,
    /// At most `TAIL_ROWS` lines, newest last.
    tail: Vec<String>,
}

/// One `WorktreeSetupSnapshot`, cleaned and bounded once when it arrives.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Setup {
    pub phase: Phase,
    started: Option<i64>,
    ended: End,
    branch: Option<String>,
    base: Option<String>,
    path: Option<String>,
    /// The setup script's name and command, from `setupScript`.
    script: Option<String>,
    command: Option<String>,
    /// Known stages only, each once, in `STAGES` order.
    stages: Vec<Stage>,
    error: Option<String>,
    /// How many changes T3 has made to the thread's setups since it last forgot them.
    pub sequence: u64,
}

impl Setup {
    /// Reads a `WorktreeSetupSnapshot` for thread `thread_id`. None when it is for another
    /// thread, or when its phase, sequence, start time or stage list is missing, of the wrong
    /// type or a phase the contract doesn't list. An optional field of the wrong type reads as
    /// absent. A stage with an id or a status the contract doesn't list is left out, and so is
    /// a second stage with the same id. Every string is cleaned and bounded here, so a frame
    /// only cuts it to the row.
    pub(super) fn parse(item: &Value, thread_id: &str) -> Option<Setup> {
        if item.get("threadId")?.as_str()? != thread_id {
            return None;
        }
        let phase = match item.get("phase")?.as_str()? {
            "running" => Phase::Running,
            "done" => Phase::Done,
            "failed" => Phase::Failed,
            "cancelled" => Phase::Cancelled,
            _ => return None,
        };
        let sequence = item.get("sequence")?.as_u64()?;
        let started = time(item.get("startedAt")?.as_str()?);
        let mut stages: Vec<Stage> = Vec::new();
        for stage in item.get("stages")?.as_array()?.iter().take(MAX_STAGES) {
            if let Some(stage) = Stage::parse(stage)
                && stages.iter().all(|known| known.index != stage.index)
            {
                stages.push(stage);
            }
        }
        stages.sort_by_key(|stage| stage.index);
        let text = |value: Option<&Value>, max: usize| {
            value
                .and_then(Value::as_str)
                .map(|text| bounded(text, max))
                .filter(|text| !text.is_empty())
        };
        let script = item.get("setupScript");
        Some(Setup {
            phase,
            started,
            ended: end(item.get("endedAt")),
            branch: text(item.get("branch"), NAME),
            base: text(item.get("baseRef"), NAME),
            path: text(item.get("worktreePath"), NAME),
            script: text(script.and_then(|script| script.get("name")), NAME),
            command: text(script.and_then(|script| script.get("command")), NAME),
            stages,
            error: text(item.get("error"), ERROR),
            sequence,
        })
    }

    /// The card's title, `headerLabel` in the desktop's card.
    fn header(&self) -> &'static str {
        match self.phase {
            Phase::Running => "Setting up worktree…",
            Phase::Done if self.any_failed() => "Worktree ready, setup script failed",
            Phase::Done => "Worktree ready",
            Phase::Failed => "Worktree setup failed",
            Phase::Cancelled => "Worktree setup cancelled",
        }
    }

    fn any_failed(&self) -> bool {
        self.stages
            .iter()
            .any(|stage| stage.status == Status::Failed)
    }

    /// Whether the agent has started, `worktreeSetupAgentStarted` in the nightly's
    /// packages/client-runtime/src/worktreeSetup.ts.
    fn agent_started(&self) -> bool {
        self.stages
            .iter()
            .any(|stage| stage.index == AGENT && stage.status == Status::Done)
    }

    fn has_details(&self) -> bool {
        self.branch.is_some()
            || self.base.is_some()
            || self.path.is_some()
            || self.command.is_some()
    }

    /// How the card shows, or None when it doesn't. Whether it shows is the nightly's
    /// `resolveVisibleWorktreeSetup` with `recorded: null`, as ChatView.tsx passes it: a running
    /// setup always shows, a follow-up retires any other, a clean finish retires once the turn
    /// is live, and a failure, a cancellation or a failed stage stays for the first turn. Once
    /// the agent has started and its turn is live, the desktop's timeline hands the setup off
    /// (`setupHandedOff` in MessagesTimeline.logic.ts): a settled one draws as one summary row
    /// and a running script moves into the working header.
    fn mode(&self, turn: Turn) -> Option<Mode> {
        let visible = match self.phase {
            Phase::Running => true,
            _ if turn.follow_up => false,
            Phase::Done => !turn.started || self.any_failed(),
            Phase::Failed | Phase::Cancelled => true,
        };
        if !visible {
            return None;
        }
        Some(match (turn.started && self.agent_started(), self.phase) {
            (false, _) => Mode::Full,
            (true, Phase::Running) => Mode::Background,
            (true, _) => Mode::Summary,
        })
    }
}

impl Stage {
    /// Reads one `WorktreeSetupStage`. None for an id or a status the contract doesn't list.
    fn parse(stage: &Value) -> Option<Stage> {
        let id = stage.get("id")?.as_str()?;
        let index = STAGES.iter().position(|(known, _)| *known == id)?;
        let status = match stage.get("status")?.as_str()? {
            "pending" => Status::Pending,
            "running" => Status::Running,
            "done" => Status::Done,
            "skipped" => Status::Skipped,
            "warning" => Status::Warning,
            "failed" => Status::Failed,
            _ => return None,
        };
        // The newest lines, read from the end, so only four are cleaned however many T3 sent.
        let mut tail: Vec<String> = stage
            .get("tail")
            .and_then(Value::as_array)
            .map(|lines| {
                lines
                    .iter()
                    .rev()
                    .filter_map(Value::as_str)
                    .take(TAIL_ROWS)
                    .map(tail_line)
                    .collect()
            })
            .unwrap_or_default();
        tail.reverse();
        Some(Stage {
            index,
            status,
            started: stage
                .get("startedAt")
                .and_then(Value::as_str)
                .and_then(time),
            ended: end(stage.get("endedAt")),
            percent: stage
                .get("percent")
                .and_then(Value::as_u64)
                .filter(|percent| *percent <= 100)
                .map(|percent| percent as u8),
            detail: stage
                .get("detail")
                .and_then(Value::as_str)
                .map(|detail| bounded(detail, DETAIL))
                .filter(|detail| !detail.is_empty()),
            tail,
        })
    }
}

/// Milliseconds since the epoch for an ISO 8601 time such as `2026-10-07T12:00:00.000Z`. None
/// when `text` doesn't start with a date and a time in range, since `parse_iso_ms` reads the
/// digits where they should be without checking the rest.
fn time(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let digits = |at: std::ops::Range<usize>| {
        bytes
            .get(at)
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_digit))
    };
    let shaped = digits(0..4)
        && bytes.get(4) == Some(&b'-')
        && digits(5..7)
        && bytes.get(7) == Some(&b'-')
        && digits(8..10)
        && matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        && digits(11..13)
        && bytes.get(13) == Some(&b':')
        && digits(14..16)
        && bytes.get(16) == Some(&b':')
        && digits(17..19);
    if !shaped {
        return None;
    }
    // The first 19 bytes are ASCII, so these slices fall between characters.
    let field = |at: std::ops::Range<usize>| text[at].parse::<u32>().unwrap_or(u32::MAX);
    let in_range = (1..=12).contains(&field(5..7))
        && (1..=31).contains(&field(8..10))
        && field(11..13) < 24
        && field(14..16) < 60
        && field(17..19) < 61;
    if in_range { parse_iso_ms(text) } else { None }
}

/// An `endedAt`: null or absent while it goes on, else its time.
fn end(value: Option<&Value>) -> End {
    match value {
        None | Some(Value::Null) => End::Open,
        Some(value) => match value.as_str().and_then(time) {
            Some(at) => End::At(at),
            None => End::Unreadable,
        },
    }
}

/// The time from `started` to the end, or to `now` while it goes on, as the desktop's
/// `stageElapsedMs` and total elapsed work it out. Never below zero and never above
/// `MAX_ELAPSED`. None without a readable start or end.
fn elapsed(started: Option<i64>, ended: End, now: i64) -> Option<String> {
    let end = match ended {
        End::Open => now,
        End::At(at) => at,
        End::Unreadable => return None,
    };
    Some(duration_label(
        end.saturating_sub(started?).clamp(0, MAX_ELAPSED),
    ))
}

/// `text` as one row of plain text: controls dropped as `without_controls` drops them, so no
/// escape sequence, bell or C1 code reaches the terminal, line breaks read as spaces, the ends
/// trimmed, and cut with `…` after `max` characters where `clusters` finds a place, so an
/// accent stays on its letter and an emoji stays whole. It reads at most four characters for
/// each it keeps, so a long string costs no more than one at the limit.
fn bounded(text: &str, max: usize) -> String {
    let (head, longer) = match text.char_indices().nth(max.saturating_mul(4)) {
        Some((at, _)) => (&text[..at], true),
        None => (text, false),
    };
    let clean = without_controls(head).replace('\n', " ");
    let clean = clean.trim();
    let mut out = String::with_capacity(clean.len().min(max * 4));
    let mut kept = 0;
    let mut cut = longer;
    for piece in clusters(clean) {
        let count = piece.chars().count();
        if kept + count > max {
            cut = true;
            break;
        }
        out.push_str(piece);
        kept += count;
    }
    if cut {
        out.truncate(out.trim_end().len());
        out.push('…');
    }
    out
}

/// One line of the setup script's output, as a terminal would leave it: the text after the
/// last carriage return that has any, so a progress bar reads as its latest state.
fn tail_line(line: &str) -> String {
    let shown = line
        .rsplit('\r')
        .find(|part| !part.trim().is_empty())
        .unwrap_or("");
    bounded(shown, TAIL_LINE)
}

/// `text` cut in the middle with `…` to at most `width` columns, as the desktop's
/// `MiddleTruncate` shows a branch or a path, so both its start and its end stay.
fn middle(text: &str, width: usize) -> String {
    let pieces = clusters(text);
    if pieces.iter().map(|piece| piece.width()).sum::<usize>() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let room = width - 1;
    let (head_room, tail_room) = (room - room / 2, room / 2);
    let mut head = String::new();
    let mut used = 0;
    for piece in &pieces {
        if used + piece.width() > head_room {
            break;
        }
        head.push_str(piece);
        used += piece.width();
    }
    let mut tail: Vec<&str> = Vec::new();
    used = 0;
    for piece in pieces.iter().rev() {
        if used + piece.width() > tail_room {
            break;
        }
        tail.push(piece);
        used += piece.width();
    }
    tail.reverse();
    format!("{head}…{}", tail.concat())
}

/// What the thread's first turn has done, which decides whether the card shows and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Turn {
    /// The thread's activity run has a `startedAt`, so the agent's turn is live, as
    /// ChatView.tsx reads `turnStarted` and MessagesTimeline reads `latestRun.startedAt`.
    pub started: bool,
    /// The transcript has more than one prompt, ChatView.tsx's `followUpSent`.
    pub follow_up: bool,
}

/// How the card draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Mode {
    /// The whole card: a title row, the stages, the script's output and the error.
    Full,
    /// One summary row, for a setup that settled with a failure after the turn went live.
    Summary,
    /// One row for a script still running after the agent started. The desktop puts a chip
    /// in the working header that opens the stages in a popover. t3term draws the row in the
    /// card's place, and Details opens the stages under it.
    Background,
}

/// The card's rows for one width.
pub(super) struct Card {
    pub lines: Vec<Line<'static>>,
    /// The Details row's index, when the card has one.
    pub toggle: Option<usize>,
    /// Whether a clock or a spinner on the card moves while the setup runs.
    pub live: bool,
}

impl Card {
    /// The rows from `from` to `to` that a frame drew, starting `top` rows below the
    /// transcript's first row on screen, which is screen row `y`. None when they are only the
    /// blank row the card ends with, or none at all.
    pub(super) fn drawn(
        &self,
        from: usize,
        to: usize,
        top: isize,
        y: u16,
        scroll: usize,
    ) -> Option<Drawn> {
        if from >= to || from + 1 >= self.lines.len() {
            return None;
        }
        Some(Drawn {
            toggle: self.toggle.map(|toggle| top + toggle as isize),
            row: self
                .toggle
                .filter(|toggle| (from..to).contains(toggle))
                .map(|toggle| y + (top + toggle as isize) as u16),
            scroll,
            live: self.live,
        })
    }
}

/// The card as the last frame drew it, for `w` and for a click on the Details row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Drawn {
    /// The Details row, counted from the transcript's first row on screen: negative above the
    /// screen, its height or more below it.
    pub toggle: Option<isize>,
    /// The Details row's screen row, while it was in view.
    pub row: Option<u16>,
    /// The scroll the frame drew.
    pub scroll: usize,
    /// Whether the rows drawn had a moving clock or spinner.
    pub live: bool,
}

/// What an item from the subscription did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    Unchanged,
    /// The held setup changed, so the card needs drawing again.
    Changed,
    /// T3 refused the subscription, which it does for a session or a server, not a thread.
    Refused,
}

/// The open thread's worktree setup: its subscription, the setup it holds and the card drawn
/// from it.
#[derive(Default)]
pub(super) struct Watch {
    /// The subscription while one is open. Dropping it ends the subscription.
    pub events: Option<mpsc::UnboundedReceiver<WatchEvent>>,
    /// Set when the subscription ended for good, so this thread doesn't ask again.
    stopped: bool,
    /// Whether the thread's activity run has a `startedAt`, as the thread's events last said.
    pub turn_started: bool,
    held: Option<Setup>,
    /// Whether the held setup came from the stream since it last connected or sent null. Only
    /// then does a snapshot with a lower sequence count as stale.
    current: bool,
    /// Counts changes to the held setup.
    revision: u64,
    /// Whether Details is open. Only this window keeps it.
    details: bool,
    /// The card's key and rows.
    card: Option<(u64, Card)>,
    pub drawn: Option<Drawn>,
    /// How many times the card was laid out, so the tests can tell.
    #[cfg(test)]
    pub layouts: usize,
}

impl Watch {
    /// Whether the thread should start a subscription: it has none and none ended for good.
    pub fn wants(&self) -> bool {
        self.events.is_none() && !self.stopped
    }

    /// Takes over a new subscription, whose first item starts a fresh comparison.
    pub fn watch(&mut self, events: mpsc::UnboundedReceiver<WatchEvent>) {
        self.events = Some(events);
        self.current = false;
    }

    /// Ends the subscription. The held setup stays, as the desktop's does.
    pub fn close(&mut self) {
        if self.events.take().is_some() {
            self.current = false;
        }
    }

    /// Applies an item from the subscription, or `None` when it ended.
    ///
    /// A snapshot for the thread replaces the held setup, unless it came after another in the
    /// same subscription and has a lower sequence, or is the same setup. Null keeps the held
    /// setup on screen, as the desktop keeps its held setup through null, so the visibility
    /// rules decide when it goes. T3 starts a thread's sequence again from zero after it sends
    /// null, 30 seconds after a setup ends, and after T3 restarts, and a reconnect may meet
    /// either. So null and a reconnect start a fresh comparison, and the next snapshot replaces
    /// the held one whatever its sequence. The desktop compares across them, and would keep an
    /// older setup with a higher sequence on screen until the new one passed it.
    pub fn apply(&mut self, event: Option<WatchEvent>, thread_id: &str) -> Outcome {
        let item = match event {
            Some(WatchEvent::Item(item)) => item,
            Some(WatchEvent::Reconnecting { .. }) => {
                self.current = false;
                return Outcome::Unchanged;
            }
            Some(WatchEvent::Failed(_)) => {
                self.stop();
                return Outcome::Refused;
            }
            None => {
                self.stop();
                return Outcome::Unchanged;
            }
        };
        if item.is_null() {
            self.current = false;
            return Outcome::Unchanged;
        }
        let Some(setup) = Setup::parse(&item, thread_id) else {
            return Outcome::Unchanged;
        };
        let stale = self.current
            && self
                .held
                .as_ref()
                .is_some_and(|held| setup.sequence < held.sequence);
        if stale {
            return Outcome::Unchanged;
        }
        self.current = true;
        if self.held.as_ref() == Some(&setup) {
            return Outcome::Unchanged;
        }
        self.held = Some(setup);
        self.revision += 1;
        // The card's rows move, so the last frame's no longer say where Details is.
        self.drawn = None;
        Outcome::Changed
    }

    fn stop(&mut self) {
        self.events = None;
        self.stopped = true;
        self.current = false;
    }

    /// Lays the card out for a frame `width` columns wide, unless the rows from last time
    /// still hold: the setup, how it shows, Details and the width are the same, and no clock
    /// on it has moved a second. A setup that isn't running has no clock to move.
    pub fn lay_out(&mut self, turn: Turn, width: usize, now: i64, theme: &Theme) {
        let Some((setup, mode)) = self
            .held
            .as_ref()
            .and_then(|setup| Some((setup, setup.mode(turn)?)))
        else {
            self.card = None;
            return;
        };
        let running = setup.phase == Phase::Running;
        let key = hash((
            self.revision,
            mode,
            self.details,
            width,
            running.then_some(now / 1000),
        ));
        if self.card.as_ref().is_some_and(|(drawn, _)| *drawn == key) {
            return;
        }
        let card = lay_out(setup, mode, self.details, width, now, theme);
        self.card = Some((key, card));
        #[cfg(test)]
        {
            self.layouts += 1;
        }
    }

    /// The card for the frame being drawn, when the setup shows.
    pub fn card(&self) -> Option<&Card> {
        self.card.as_ref().map(|(_, card)| card)
    }

    /// Opens or closes Details while the last frame drew the card with a Details row, and
    /// returns the row that row should keep in the next frame. Opening keeps it where it is,
    /// on screen or not. Closing brings a row above the screen to the top, so the shorter card
    /// doesn't leave an unrelated block in view. The frame's record goes with the toggle, so a
    /// second toggle before the next frame does nothing.
    pub fn toggle(&mut self) -> Option<isize> {
        let row = self.drawn?.toggle?;
        self.drawn = None;
        self.details = !self.details;
        Some(if self.details { row } else { row.max(0) })
    }

    /// Whether screen row `row` showed the Details row in the last frame, and the transcript
    /// hasn't scrolled since.
    pub fn toggle_at(&self, row: u16, scroll: usize) -> bool {
        self.drawn
            .is_some_and(|drawn| drawn.row == Some(row) && drawn.scroll == scroll)
    }

    /// Whether Details is open, while the last frame drew a card with a Details row.
    pub fn hint(&self) -> Option<bool> {
        self.drawn
            .and_then(|drawn| drawn.toggle)
            .map(|_| self.details)
    }

    /// Whether the card needs the once-a-second tick: the last frame drew its moving clock or
    /// spinner, and the subscription is open and still says the setup runs. A retained setup
    /// that finished, a card out of view, a closed or refused subscription, and a running setup
    /// held through a null or a reconnect don't tick.
    pub fn ticks(&self) -> bool {
        self.events.is_some()
            && self.current
            && self
                .held
                .as_ref()
                .is_some_and(|held| held.phase == Phase::Running)
            && self.drawn.is_some_and(|drawn| drawn.live)
    }
}

/// The card's rows. Under the stages and the script's output come the error, for a failed
/// setup, and a Details row when there is anything to show, with the branch, base, path and
/// script command under it while it is open. A script still running after the agent started
/// has one row, and its Details row opens the stages too. The desktop shows the details above
/// its Details button, and t3term shows everything Details opens below its row, so the row
/// doesn't move when it opens. Every row is cut to `width`.
fn lay_out(setup: &Setup, mode: Mode, details: bool, width: usize, now: i64, t: &Theme) -> Card {
    let muted = Style::new().fg(t.muted);
    let total = elapsed(setup.started, setup.ended, now);
    let mut lines = Vec::new();
    match mode {
        Mode::Full => {
            lines.push(title_row(setup, total, width, now, t));
            lines.push(Line::styled("─".repeat(width), Style::new().fg(t.border)));
            stage_rows(&mut lines, setup, width, now, t);
        }
        Mode::Summary => lines.push(summary_row(setup, total, width, t)),
        Mode::Background => lines.push(background_row(setup, width, now, t)),
    }
    if setup.phase == Phase::Failed
        && let Some(error) = &setup.error
    {
        let mut rows = tasks::wrap(error, width.saturating_sub(2).max(1));
        if rows.len() > ERROR_ROWS {
            rows.truncate(ERROR_ROWS);
            if let Some(last) = rows.last_mut() {
                *last = clip(&format!("{last}…"), width.saturating_sub(2));
            }
        }
        for text in rows {
            lines.push(fitted(
                vec![("  ".into(), muted), (text, muted)],
                None,
                width,
            ));
        }
    }
    let toggle = (mode == Mode::Background || setup.has_details()).then(|| {
        let chevron = if details { "⌄ " } else { "› " };
        lines.push(fitted(
            vec![
                (chevron.into(), Style::new().fg(t.border_strong)),
                ("Details".into(), muted),
            ],
            None,
            width,
        ));
        lines.len() - 1
    });
    if details {
        if mode == Mode::Background {
            stage_rows(&mut lines, setup, width, now, t);
        }
        detail_rows(&mut lines, setup, width, t);
    }
    lines.push(Line::default());
    Card {
        lines,
        toggle,
        live: setup.phase == Phase::Running,
    }
}

/// `left` and then `right` at the end of a row exactly `width` columns wide. Each part of
/// `left` is cut to the columns the parts before it leave. `right` goes when it would leave
/// `left` fewer than eight.
fn fitted(
    left: Vec<(String, Style)>,
    right: Option<(String, Style)>,
    width: usize,
) -> Line<'static> {
    let right = right.filter(|(text, _)| text.width() + 9 <= width);
    let mut room = match &right {
        Some((text, _)) => width - text.width() - 1,
        None => width,
    };
    let mut spans = Vec::new();
    for (text, style) in left {
        if room == 0 {
            break;
        }
        let text = clip(&text, room);
        room = room.saturating_sub(text.width());
        spans.push(Span::styled(text, style));
    }
    let right = right
        .map(|(text, style)| vec![Span::styled(text, style)])
        .unwrap_or_default();
    row(spans, right, width)
}

/// The title row, which the desktop's `SetupHeaderRow` draws for a failed or cancelled setup
/// and its working row for the rest: the card's title with the total time at the end.
fn title_row(
    setup: &Setup,
    total: Option<String>,
    width: usize,
    now: i64,
    t: &Theme,
) -> Line<'static> {
    let style = match setup.phase {
        Phase::Running => Style::new().fg(t.info),
        Phase::Failed => Style::new().fg(t.error_fg),
        Phase::Done if setup.any_failed() => Style::new().fg(t.warning_fg),
        Phase::Done | Phase::Cancelled => Style::new().fg(t.muted),
    };
    let mut left = Vec::new();
    if setup.phase == Phase::Running {
        left.push((format!("{} ", spinner_frame(now)), style));
    }
    left.push((setup.header().to_string(), style));
    fitted(
        left,
        total.map(|total| (total, Style::new().fg(t.muted))),
        width,
    )
}

/// The stages, each as a row like a tool call's, with the script's output under its row while
/// it runs with output and after it fails.
fn stage_rows(lines: &mut Vec<Line<'static>>, setup: &Setup, width: usize, now: i64, t: &Theme) {
    for stage in &setup.stages {
        lines.push(stage_row(stage, setup.script.as_deref(), width, now, t));
        let shows_tail = stage.index == SCRIPT
            && (stage.status == Status::Failed
                || (stage.status == Status::Running && !stage.tail.is_empty()));
        if shows_tail {
            tail_rows(lines, &stage.tail, stage.status == Status::Failed, width, t);
        }
    }
}

/// One stage, as the desktop's `StageRow` draws it: an icon for its status, its label, which
/// is the script's name for the script, what it is doing, and how long it took. A pending stage
/// says nothing more, a skipped one says why or `skipped`, and the checkout says how far it got
/// while it runs. Neither has a time.
fn stage_row(
    stage: &Stage,
    script: Option<&str>,
    width: usize,
    now: i64,
    t: &Theme,
) -> Line<'static> {
    let muted = Style::new().fg(t.muted);
    let (icon, style) = match stage.status {
        Status::Done => ("✓", muted),
        Status::Running => (spinner_frame(now), Style::new().fg(t.info)),
        Status::Failed => ("✕", Style::new().fg(t.error_fg)),
        Status::Warning => ("!", Style::new().fg(t.warning_fg)),
        Status::Skipped => ("−", muted),
        Status::Pending => ("○", muted.add_modifier(Modifier::DIM)),
    };
    let label = match script {
        Some(name) if stage.index == SCRIPT => name,
        _ => STAGES[stage.index].1,
    };
    let trailing = match stage.status {
        Status::Pending => None,
        Status::Skipped => Some(stage.detail.as_deref().unwrap_or("skipped").to_string()),
        Status::Running if stage.index == CHECKOUT && stage.percent.is_some() => {
            stage.percent.map(|percent| format!("{percent}%"))
        }
        _ => stage.detail.clone(),
    };
    let time = match stage.status {
        Status::Pending | Status::Skipped => None,
        _ => elapsed(stage.started, stage.ended, now),
    };
    // The label keeps at most half the row when something follows it, as a tool call's does.
    let label = match &trailing {
        Some(_) => clip(label, (width / 2).max(12)),
        None => label.to_string(),
    };
    let mut left = vec![(format!("{icon} "), style), (label, style)];
    if let Some(trailing) = trailing {
        left.push((format!("  {trailing}"), muted));
    }
    fitted(left, time.map(|time| (time, muted)), width)
}

/// The script's last lines in four rows, newest last and blank rows first, so the card grows
/// once when output starts and not with each line, as the desktop's `OutputTail` does. A row
/// never wraps. Red after the script failed.
fn tail_rows(
    lines: &mut Vec<Line<'static>>,
    tail: &[String],
    failed: bool,
    width: usize,
    t: &Theme,
) {
    let style = Style::new().fg(if failed { t.error_fg } else { t.muted });
    let edge = Style::new().fg(t.border_strong);
    let blank = TAIL_ROWS.saturating_sub(tail.len());
    for slot in 0..TAIL_ROWS {
        let text = slot
            .checked_sub(blank)
            .and_then(|at| tail.get(at))
            .cloned()
            .unwrap_or_default();
        lines.push(fitted(
            vec![("  │ ".into(), edge), (text, style)],
            None,
            width,
        ));
    }
}

/// The desktop's `CollapsedSummaryRow`: the outcome with the total time, failed for a failed or
/// cancelled setup and for a failed script, done otherwise.
fn summary_row(setup: &Setup, total: Option<String>, width: usize, t: &Theme) -> Line<'static> {
    let failed = matches!(setup.phase, Phase::Failed | Phase::Cancelled)
        || setup
            .stages
            .iter()
            .any(|stage| stage.index == SCRIPT && stage.status == Status::Failed);
    let (icon, style) = if failed {
        ("✕", Style::new().fg(t.error_fg))
    } else {
        ("✓", Style::new().fg(t.muted))
    };
    let left = vec![
        (format!("{icon} "), style),
        (setup.header().to_string(), style),
    ];
    fitted(
        left,
        total.map(|total| (total, Style::new().fg(t.muted))),
        width,
    )
}

/// The row for a script still running after the agent started, in place of the desktop's
/// `BackgroundWorktreeSetupChip`: a spinner, the script's name or `Setup script`, and how long
/// it has run.
fn background_row(setup: &Setup, width: usize, now: i64, t: &Theme) -> Line<'static> {
    let info = Style::new().fg(t.info);
    let muted = Style::new().fg(t.muted);
    let name = setup.script.as_deref().unwrap_or("Setup script");
    let time = setup
        .stages
        .iter()
        .find(|stage| stage.index == SCRIPT)
        .and_then(|stage| elapsed(stage.started, stage.ended, now));
    let left = vec![
        (format!("{} ", spinner_frame(now)), info),
        (name.to_string(), info),
        (" is still running".to_string(), muted),
    ];
    fitted(left, time.map(|time| (time, muted)), width)
}

/// The desktop's `SetupDetails`: the branch, base and path cut in the middle, and the script's
/// command cut at the end, each when T3 sent it.
fn detail_rows(lines: &mut Vec<Line<'static>>, setup: &Setup, width: usize, t: &Theme) {
    let fields = [
        ("Branch", &setup.branch, true),
        ("Base", &setup.base, true),
        ("Path", &setup.path, true),
        ("Setup", &setup.command, false),
    ];
    let label_width = fields
        .iter()
        .filter(|(_, value, _)| value.is_some())
        .map(|(label, _, _)| label.len())
        .max()
        .unwrap_or(0);
    let room = width.saturating_sub(2 + label_width + 2);
    for (label, value, in_middle) in fields {
        let Some(value) = value else {
            continue;
        };
        let shown = if in_middle {
            middle(value, room)
        } else {
            value.clone()
        };
        lines.push(fitted(
            vec![
                ("  ".into(), Style::new()),
                (format!("{label:<label_width$}  "), Style::new().fg(t.fg)),
                (shown, Style::new().fg(t.muted)),
            ],
            None,
            width,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::super::theme::Depth;
    use super::*;
    use serde_json::json;

    const START: &str = "2026-10-07T00:00:00.000Z";
    /// The turn before it starts, live, and after a follow-up.
    const BEFORE: Turn = Turn {
        started: false,
        follow_up: false,
    };
    const LIVE: Turn = Turn {
        started: true,
        follow_up: false,
    };
    const FOLLOW_UP: Turn = Turn {
        started: true,
        follow_up: true,
    };

    /// The clock the cards here are drawn at, 30 seconds after `START`.
    fn now() -> i64 {
        time("2026-10-07T00:00:30.000Z").expect("a time")
    }

    fn stage(id: &str, status: &str) -> Value {
        json!({"id": id, "status": status, "startedAt": null, "endedAt": null,
            "percent": null, "detail": null, "tail": []})
    }

    /// A stage that took 1.2 seconds.
    fn finished(id: &str, status: &str) -> Value {
        let mut stage = stage(id, status);
        stage["startedAt"] = json!(START);
        stage["endedAt"] = json!("2026-10-07T00:00:01.200Z");
        stage
    }

    /// The five stages in order, with `statuses`.
    fn stages(statuses: [&str; 5]) -> Vec<Value> {
        STAGES
            .iter()
            .zip(statuses)
            .map(|((id, _), status)| stage(id, status))
            .collect()
    }

    /// Thread `t`'s setup as T3 sends it.
    fn snapshot(phase: &str, sequence: u64, stages: Vec<Value>) -> Value {
        json!({"threadId": "t", "phase": phase, "startedAt": START, "endedAt": null,
            "branch": "t3code/fix-login", "baseRef": "main", "worktreePath": "/work/repo-t1",
            "setupScript": {"name": "Install", "command": "bun install", "terminalId": "setup-1"},
            "stages": stages, "error": null, "sequence": sequence})
    }

    fn setup(phase: &str, stages: Vec<Value>) -> Setup {
        Setup::parse(&snapshot(phase, 1, stages), "t").expect("a setup")
    }

    /// The card's rows as text, `width` columns wide, as of `now()`.
    fn card_rows(setup: &Setup, turn: Turn, details: bool, width: usize) -> Vec<String> {
        let mode = setup.mode(turn).expect("the card shows");
        let card = lay_out(
            setup,
            mode,
            details,
            width,
            now(),
            &Theme::new(Depth::TrueColor),
        );
        card.lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    /// Each row without the spaces that pad it to the width.
    fn trimmed(rows: &[String]) -> Vec<&str> {
        rows.iter().map(|row| row.trim_end()).collect()
    }

    #[test]
    fn a_snapshot_must_be_for_the_thread_and_well_formed() {
        let good = json!({"threadId": "t", "phase": "running", "startedAt": "2026-10-07T00:00:00.000Z",
            "endedAt": null, "branch": null, "baseRef": null, "worktreePath": null,
            "setupScript": null, "stages": [], "error": null, "sequence": 0});
        assert!(Setup::parse(&good, "t").is_some());
        assert!(Setup::parse(&good, "other").is_none());
        let broken = |key: &str, value: Value| {
            let mut item = good.clone();
            item[key] = value;
            Setup::parse(&item, "t")
        };
        assert!(broken("phase", json!("paused")).is_none());
        assert!(broken("phase", json!(1)).is_none());
        assert!(broken("sequence", json!(-1)).is_none());
        assert!(broken("sequence", json!("3")).is_none());
        assert!(broken("stages", json!({})).is_none());
        assert!(broken("startedAt", Value::Null).is_none());
        assert!(broken("threadId", json!(7)).is_none());
        assert!(Setup::parse(&json!([]), "t").is_none());
        assert!(Setup::parse(&json!("t"), "t").is_none());
        // An optional field of the wrong type reads as absent.
        let odd = broken("branch", json!(["main"])).expect("still a setup");
        assert_eq!(odd.branch, None);
        let odd = broken("setupScript", json!("bun install")).expect("still a setup");
        assert_eq!((odd.script, odd.command), (None, None));
        // A start time that isn't one leaves the setup without a clock.
        let odd = broken("startedAt", json!("yesterday")).expect("still a setup");
        assert_eq!(odd.started, None);
        assert_eq!(elapsed(odd.started, odd.ended, 0), None);
    }

    #[test]
    fn stages_keep_the_known_ids_once_in_order() {
        let item = json!({"threadId": "t", "phase": "running", "startedAt": "2026-10-07T00:00:00Z",
        "sequence": 1, "stages": [
            {"id": "agent", "status": "pending", "tail": []},
            {"id": "deploy", "status": "running", "tail": []},
            {"id": "fetch", "status": "done", "tail": []},
            {"id": "fetch", "status": "failed", "tail": []},
            {"id": "checkout", "status": "spinning", "tail": []},
            {"id": "checkout", "status": "running", "percent": 140, "tail": []},
            "setup-script",
        ]});
        let setup = Setup::parse(&item, "t").expect("a setup");
        let read: Vec<(usize, Status)> = setup.stages.iter().map(|s| (s.index, s.status)).collect();
        assert_eq!(
            read,
            [
                (0, Status::Done),
                (CHECKOUT, Status::Running),
                (AGENT, Status::Pending)
            ]
        );
        assert_eq!(setup.stages[1].percent, None, "past 100 isn't a percentage");
    }

    #[test]
    fn times_must_be_dates() {
        assert_eq!(time("2026-10-07T00:00:01.500Z"), Some(1_791_331_201_500));
        for bad in [
            "",
            "2026-10-07",
            "2026-13-07T00:00:00Z",
            "2026-10-07T25:00:00Z",
            "２０２６-10-07T00:00:00Z",
            "2026-1é-07T00:00:00Z",
        ] {
            assert_eq!(time(bad), None, "{bad}");
        }
        // Ends before starts, and far ends, are clamped.
        assert_eq!(
            elapsed(Some(5_000), End::At(1_000), 0).as_deref(),
            Some("0.0s")
        );
        assert_eq!(
            elapsed(Some(0), End::At(i64::MAX), 0),
            Some(duration_label(MAX_ELAPSED))
        );
        assert_eq!(elapsed(Some(0), End::Unreadable, 10), None);
        assert_eq!(elapsed(Some(0), End::Open, 2_000).as_deref(), Some("2.0s"));
    }

    #[test]
    fn text_is_one_clean_bounded_row() {
        let dirty = "\u{1b}]52;c;aGk=\u{7}main\u{1b}[2J\r\n\u{9b}31mbranch\u{7f}\u{85}\ttail";
        let clean = bounded(dirty, 100);
        assert!(!clean.chars().any(char::is_control), "{clean:?}");
        assert!(clean.contains("main") && clean.contains("branch") && clean.contains("tail"));
        // Cut after `max` characters, at a place that keeps a grapheme whole.
        assert_eq!(bounded("abcdef", 3), "abc…");
        assert_eq!(bounded("e\u{301}e\u{301}e\u{301}", 3), "e\u{301}…");
        assert_eq!(bounded("👩‍💻👩‍💻", 4), "👩‍💻…");
        assert_eq!(bounded("漢字かな", 3), "漢字か…");
        // A huge string costs what one at the limit does, and still says it was cut.
        let huge = "x".repeat(10_000_000);
        assert_eq!(bounded(&huge, 5), "xxxxx…");
        // A progress bar reads as its last state.
        assert_eq!(tail_line("10%\r50%\r100%\r"), "100%");
        assert_eq!(tail_line("\r\r"), "");
    }

    #[test]
    fn middle_cuts_keep_both_ends() {
        assert_eq!(middle("/work/repo-t1", 20), "/work/repo-t1");
        assert_eq!(
            middle("/Users/someone/worktrees/repo/feature", 11),
            "/User…ature"
        );
        assert_eq!(middle("漢字漢字漢字", 7), "漢…字");
        assert_eq!(middle("abc", 1), "…");
        assert_eq!(middle("abc", 0), "");
    }

    #[test]
    fn the_card_shows_and_retires_as_the_desktop_decides() {
        // A running setup always shows. Once the agent started and its turn is live, a script
        // still running is one row.
        let running = setup(
            "running",
            stages(["done", "done", "done", "running", "done"]),
        );
        assert_eq!(running.mode(BEFORE), Some(Mode::Full));
        assert_eq!(running.mode(LIVE), Some(Mode::Background));
        assert_eq!(running.mode(FOLLOW_UP), Some(Mode::Background));
        // A live turn alone doesn't hand the setup off. The agent's stage has to be done too.
        let checking_out = setup(
            "running",
            stages(["done", "running", "pending", "pending", "pending"]),
        );
        assert_eq!(checking_out.mode(LIVE), Some(Mode::Full));
        // A clean finish shows until the turn is live, then retires.
        let ready = setup("done", stages(["done", "done", "skipped", "done", "done"]));
        assert_eq!(ready.mode(BEFORE), Some(Mode::Full));
        assert_eq!(ready.mode(LIVE), None);
        // A failed script stays as one row for the first turn and goes with a follow-up.
        let script_failed = setup(
            "done",
            stages(["done", "done", "skipped", "failed", "done"]),
        );
        assert_eq!(script_failed.mode(BEFORE), Some(Mode::Full));
        assert_eq!(script_failed.mode(LIVE), Some(Mode::Summary));
        assert_eq!(script_failed.mode(FOLLOW_UP), None);
        // A failed or cancelled setup never started the agent, so it stays whole for the first
        // turn, and a follow-up retires it too.
        for phase in ["failed", "cancelled"] {
            let stopped = setup(
                phase,
                stages(["done", "failed", "pending", "pending", "pending"]),
            );
            assert_eq!(stopped.mode(BEFORE), Some(Mode::Full));
            assert_eq!(stopped.mode(LIVE), Some(Mode::Full));
            assert_eq!(stopped.mode(FOLLOW_UP), None);
        }
        assert_eq!(
            setup("failed", stages(["done", "done", "done", "done", "done"])).mode(LIVE),
            Some(Mode::Summary)
        );
    }

    #[test]
    fn stages_read_as_the_desktop_labels_them() {
        let mut checkout = stage("checkout", "running");
        checkout["percent"] = json!(42);
        checkout["detail"] = json!("Updating files");
        let mut warned = finished("submodules", "warning");
        warned["detail"] = json!("1 submodule skipped");
        let progress = setup(
            "running",
            vec![
                finished("fetch", "done"),
                checkout,
                warned,
                stage("setup-script", "pending"),
                stage("agent", "pending"),
            ],
        );
        let rows = card_rows(&progress, BEFORE, false, 60);
        let spinner = spinner_frame(now());
        assert!(rows[0].starts_with(&format!("{spinner} Setting up worktree…")));
        assert!(rows[0].ends_with(" 30s"), "{:?}", rows[0]);
        assert_eq!(rows[1], "─".repeat(60));
        assert!(rows[2].starts_with("✓ Fetch base branch") && rows[2].ends_with(" 1.2s"));
        // The running checkout says how far it got rather than what it is doing.
        assert!(rows[3].contains("Check out files  42%") && !rows[3].contains("Updating"));
        assert!(rows[4].starts_with("! Init submodules  1 submodule skipped"));
        assert!(rows[4].ends_with(" 1.2s"));
        // A pending stage says nothing more, and the script's row is the script's name.
        assert_eq!(
            trimmed(&rows[5..]),
            ["○ Install", "○ Start agent", "› Details", ""]
        );
        assert!(rows.iter().all(|row| row.width() <= 60));

        // Only a running checkout shows a percentage. A finished one says what it did, and a
        // skipped stage says why, or `skipped`, with no time.
        let mut fetching = stage("fetch", "running");
        fetching["percent"] = json!(50);
        let mut checked_out = finished("checkout", "done");
        checked_out["percent"] = json!(100);
        checked_out["detail"] = json!("Checked out 120 files");
        let later = setup(
            "running",
            vec![fetching, checked_out, finished("submodules", "skipped")],
        );
        let rows = card_rows(&later, BEFORE, false, 60);
        assert_eq!(rows[2].trim_end(), format!("{spinner} Fetch base branch"));
        assert!(rows[3].starts_with("✓ Check out files  Checked out 120 files"));
        assert!(!rows[3].contains('%'));
        assert_eq!(rows[4].trim_end(), "− Init submodules  skipped");
    }

    #[test]
    fn the_script_output_keeps_four_rows() {
        let script = |status: &str, tail: Vec<String>| {
            let mut script = stage("setup-script", status);
            script["tail"] = json!(tail);
            let phase = if status == "failed" {
                "done"
            } else {
                "running"
            };
            setup(phase, vec![script])
        };
        let lines = |lines: &[&str]| lines.iter().map(|line| line.to_string()).collect();
        // Output fills the box from the bottom, newest last.
        let rows = card_rows(
            &script("running", lines(&["resolving", "installing"])),
            BEFORE,
            false,
            40,
        );
        assert_eq!(
            trimmed(&rows[3..7]),
            ["  │", "  │", "  │ resolving", "  │ installing"]
        );
        // A script running without output yet, or done, has no box.
        let rows = card_rows(&script("running", Vec::new()), BEFORE, false, 40);
        assert_eq!(rows.len(), 5);
        let rows = card_rows(&script("done", lines(&["ok"])), BEFORE, false, 40);
        assert_eq!(rows.len(), 5);
        // A failed script keeps its box, empty or with its newest four lines.
        let rows = card_rows(&script("failed", Vec::new()), BEFORE, false, 40);
        assert!(rows[0].starts_with("Worktree ready, setup script failed"));
        assert_eq!(trimmed(&rows[3..7]), ["  │"; 4]);
        let many: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let rows = card_rows(&script("failed", many), BEFORE, false, 40);
        assert_eq!(
            trimmed(&rows[3..7]),
            ["  │ line 46", "  │ line 47", "  │ line 48", "  │ line 49"]
        );
        // A long line is cut to its row, never wrapped.
        let rows = card_rows(
            &script("failed", vec!["x".repeat(10_000)]),
            BEFORE,
            false,
            40,
        );
        assert_eq!(rows.len(), 9);
        assert!(
            rows[6].ends_with('…') && rows[6].width() == 40,
            "{:?}",
            rows[6]
        );
    }

    #[test]
    fn an_error_takes_six_rows_at_most() {
        let mut failed = snapshot(
            "failed",
            1,
            stages(["done", "failed", "pending", "pending", "pending"]),
        );
        failed["error"] = json!("word ".repeat(1000));
        let failed = Setup::parse(&failed, "t").expect("a setup");
        let rows = card_rows(&failed, BEFORE, false, 40);
        assert!(rows[0].starts_with("Worktree setup failed"));
        let error: Vec<&String> = rows
            .iter()
            .filter(|row| row.starts_with("  word"))
            .collect();
        assert_eq!(error.len(), ERROR_ROWS);
        assert!(error[ERROR_ROWS - 1].trim_end().ends_with('…'));
        // A setup that didn't fail keeps its error to itself, as the desktop's does.
        let mut done = snapshot("done", 1, Vec::new());
        done["error"] = json!("stale");
        let done = Setup::parse(&done, "t").expect("a setup");
        assert!(
            card_rows(&done, BEFORE, false, 40)
                .iter()
                .all(|row| !row.contains("stale"))
        );
    }

    #[test]
    fn details_open_under_their_row() {
        let ready = setup("done", stages(["done", "done", "skipped", "done", "done"]));
        let closed = card_rows(&ready, BEFORE, false, 50);
        let open = card_rows(&ready, BEFORE, true, 50);
        let at = closed
            .iter()
            .position(|row| row.starts_with("› Details"))
            .expect("a Details row");
        assert_eq!(open[..at], closed[..at]);
        assert!(open[at].starts_with("⌄ Details"));
        assert_eq!(
            trimmed(&open[at + 1..]),
            [
                "  Branch  t3code/fix-login",
                "  Base    main",
                "  Path    /work/repo-t1",
                "  Setup   bun install",
                "",
            ]
        );
        // A long path keeps both of its ends.
        let mut far = snapshot("done", 1, Vec::new());
        far["worktreePath"] = json!("/Users/someone/.t3/worktrees/repository/feature-branch-name");
        let far = Setup::parse(&far, "t").expect("a setup");
        let rows = card_rows(&far, BEFORE, true, 30);
        let path = rows
            .iter()
            .find(|row| row.starts_with("  Path"))
            .expect("a path row");
        assert!(path.starts_with("  Path    /Users/"), "{path}");
        assert!(
            path.trim_end().ends_with("-name") && path.contains('…'),
            "{path}"
        );
        assert!(path.width() <= 30);
        // With nothing to show, a settled card has no Details row.
        let mut bare = snapshot("done", 1, Vec::new());
        for key in ["branch", "baseRef", "worktreePath", "setupScript"] {
            bare[key] = Value::Null;
        }
        let bare = Setup::parse(&bare, "t").expect("a setup");
        assert!(
            card_rows(&bare, BEFORE, false, 50)
                .iter()
                .all(|row| !row.contains("Details"))
        );
    }

    #[test]
    fn a_script_running_after_the_handoff_is_one_row() {
        let mut script = stage("setup-script", "running");
        script["startedAt"] = json!("2026-10-07T00:00:10.000Z");
        script["tail"] = json!(["compiling"]);
        let background = setup(
            "running",
            vec![
                finished("fetch", "done"),
                finished("checkout", "done"),
                script,
                finished("agent", "done"),
            ],
        );
        let rows = card_rows(&background, LIVE, false, 50);
        let spinner = spinner_frame(now());
        assert!(rows[0].starts_with(&format!("{spinner} Install is still running")));
        assert!(rows[0].ends_with(" 20s"), "{:?}", rows[0]);
        assert_eq!(trimmed(&rows[1..]), ["› Details", ""]);
        // Details opens the stages and the script's output as well, all under the row.
        let rows = card_rows(&background, LIVE, true, 50);
        assert!(rows[1].starts_with("⌄ Details"));
        assert!(rows[2].starts_with("✓ Fetch base branch"));
        assert!(rows.iter().any(|row| row.trim_end() == "  │ compiling"));
        assert!(
            rows.iter()
                .any(|row| row.starts_with("  Setup   bun install"))
        );
        // Without a script name it is the setup script.
        let mut unnamed = snapshot("running", 1, vec![finished("agent", "done")]);
        unnamed["setupScript"] = Value::Null;
        let unnamed = Setup::parse(&unnamed, "t").expect("a setup");
        let rows = card_rows(&unnamed, LIVE, false, 50);
        assert!(rows[0].contains("Setup script is still running"));
    }

    #[test]
    fn every_row_fits_its_width() {
        let mut item = snapshot(
            "failed",
            1,
            stages(["done", "running", "warning", "failed", "skipped"]),
        );
        item["stages"][1]["percent"] = json!(42);
        item["stages"][2]["detail"] = json!("詳細な説明 👩\u{200d}💻 e\u{301}");
        item["stages"][3]["tail"] = json!(["x".repeat(500), "漢字".repeat(300)]);
        item["error"] = json!("failed ".repeat(200));
        item["branch"] = json!("枝".repeat(300));
        let setup = Setup::parse(&item, "t").expect("a setup");
        for width in 1..=90 {
            for details in [false, true] {
                for row in card_rows(&setup, BEFORE, details, width) {
                    assert!(row.width() <= width, "{width}: {row:?}");
                }
            }
        }
    }

    /// A running setup with its checkout at `percent`, as an item from the subscription.
    fn item(sequence: u64, percent: u64) -> Option<WatchEvent> {
        let mut checkout = stage("checkout", "running");
        checkout["percent"] = json!(percent);
        Some(WatchEvent::Item(snapshot(
            "running",
            sequence,
            vec![checkout],
        )))
    }

    fn percent(watch: &Watch) -> Option<u8> {
        watch.held.as_ref()?.stages.first()?.percent
    }

    #[test]
    fn a_subscription_holds_the_newest_setup_it_has_seen() {
        let mut watch = Watch::default();
        assert!(watch.wants());
        watch.watch(mpsc::unbounded_channel().1);
        assert!(!watch.wants());
        assert_eq!(watch.apply(item(5, 40), "t"), Outcome::Changed);
        // A lower sequence in the same subscription is older news, and the same setup again is
        // no news.
        assert_eq!(watch.apply(item(3, 10), "t"), Outcome::Unchanged);
        assert_eq!(watch.apply(item(5, 40), "t"), Outcome::Unchanged);
        assert_eq!(percent(&watch), Some(40));
        assert_eq!(watch.apply(item(6, 60), "t"), Outcome::Changed);
        // Another thread's setup, or one that isn't a setup, changes nothing.
        let mut other = snapshot("running", 9, Vec::new());
        other["threadId"] = json!("u");
        assert_eq!(
            watch.apply(Some(WatchEvent::Item(other)), "t"),
            Outcome::Unchanged
        );
        let broken = WatchEvent::Item(json!({"threadId": "t", "sequence": 9}));
        assert_eq!(watch.apply(Some(broken), "t"), Outcome::Unchanged);
        assert_eq!(percent(&watch), Some(60));
        // Null keeps the setup, and T3 counts from zero again after it.
        let null = || Some(WatchEvent::Item(Value::Null));
        assert_eq!(watch.apply(null(), "t"), Outcome::Unchanged);
        assert_eq!(percent(&watch), Some(60));
        assert_eq!(watch.apply(item(0, 5), "t"), Outcome::Changed);
        assert_eq!(watch.apply(item(1, 15), "t"), Outcome::Changed);
        // A reconnect may meet a T3 that restarted, so its first setup replaces the held one too.
        let reconnecting = WatchEvent::Reconnecting {
            reason: "closed".into(),
            retry_in: std::time::Duration::from_millis(250),
        };
        assert_eq!(watch.apply(Some(reconnecting), "t"), Outcome::Unchanged);
        assert_eq!(percent(&watch), Some(15));
        assert_eq!(watch.apply(item(0, 1), "t"), Outcome::Changed);
        assert_eq!(percent(&watch), Some(1));
        assert_eq!(watch.apply(item(0, 1), "t"), Outcome::Unchanged);
        // A refusal stops the subscription for good, and so does its end.
        let refused = WatchEvent::Failed("unknown method".into());
        assert_eq!(watch.apply(Some(refused), "t"), Outcome::Refused);
        assert!(watch.events.is_none() && !watch.wants());
        assert_eq!(percent(&watch), Some(1), "the card stays as it was");
        let mut ended = Watch::default();
        ended.watch(mpsc::unbounded_channel().1);
        assert_eq!(ended.apply(None, "t"), Outcome::Unchanged);
        assert!(!ended.wants());
        // Closed, as when the thread loses its worktree, it can open again.
        let mut closed = Watch::default();
        closed.watch(mpsc::unbounded_channel().1);
        closed.close();
        assert!(closed.events.is_none() && closed.wants());
    }

    #[test]
    fn only_a_running_card_on_screen_ticks() {
        let theme = Theme::new(Depth::TrueColor);
        // A frame that lays the card out at `now` and draws all of it.
        let draw = |watch: &mut Watch, now: i64| {
            watch.lay_out(BEFORE, 60, now, &theme);
            watch.drawn = watch
                .card()
                .and_then(|card| card.drawn(0, card.lines.len(), 0, 0, 0));
        };
        let mut watch = Watch::default();
        watch.watch(mpsc::unbounded_channel().1);
        watch.apply(item(1, 40), "t");
        watch.lay_out(BEFORE, 60, now(), &theme);
        assert!(!watch.ticks(), "not drawn yet");
        draw(&mut watch, now());
        assert!(watch.ticks());
        // The clock moves once a second, so a frame within the second lays nothing out.
        let layouts = watch.layouts;
        draw(&mut watch, now() + 400);
        assert_eq!(watch.layouts, layouts);
        draw(&mut watch, now() + 1000);
        assert_eq!(watch.layouts, layouts + 1);
        // The blank row the card ends with isn't the card.
        let card = watch.card().expect("a card");
        let last = card.lines.len();
        assert_eq!(card.drawn(last - 1, last, 0, 0, 0), None);
        // Out of view it doesn't tick.
        watch.drawn = None;
        assert!(!watch.ticks());
        // A setup that ended keeps its card with no clock to move.
        let done = snapshot("done", 2, vec![finished("checkout", "done")]);
        assert_eq!(
            watch.apply(Some(WatchEvent::Item(done)), "t"),
            Outcome::Changed
        );
        draw(&mut watch, now());
        assert!(watch.card().is_some() && !watch.ticks());
        let layouts = watch.layouts;
        draw(&mut watch, now() + 5000);
        assert_eq!(watch.layouts, layouts);
        let failed = snapshot("failed", 3, vec![finished("checkout", "failed")]);
        assert_eq!(
            watch.apply(Some(WatchEvent::Item(failed)), "t"),
            Outcome::Changed
        );
        draw(&mut watch, now());
        assert!(watch.card().is_some() && !watch.ticks());
        // A running setup held through a null, or with its subscription closed, doesn't tick.
        watch.apply(item(4, 70), "t");
        draw(&mut watch, now());
        assert!(watch.ticks());
        watch.apply(Some(WatchEvent::Item(Value::Null)), "t");
        assert!(!watch.ticks());
        watch.apply(item(0, 80), "t");
        draw(&mut watch, now());
        assert!(watch.ticks());
        watch.close();
        assert!(!watch.ticks());
        // A new width or Details lays it out again.
        let layouts = watch.layouts;
        watch.lay_out(BEFORE, 61, now(), &theme);
        assert_eq!(watch.layouts, layouts + 1);
        watch.details = true;
        watch.lay_out(BEFORE, 61, now(), &theme);
        assert_eq!(watch.layouts, layouts + 2);
    }

    #[test]
    fn details_toggle_only_from_a_drawn_row() {
        let drawn = |toggle, row| Drawn {
            toggle,
            row,
            scroll: 7,
            live: false,
        };
        let mut watch = Watch::default();
        assert_eq!((watch.toggle(), watch.hint()), (None, None));
        watch.drawn = Some(drawn(Some(-3), None));
        assert_eq!(watch.hint(), Some(false));
        // Opening keeps the row where it was, even above the screen. The frame's record goes
        // with it, so a second press before the next frame does nothing.
        assert_eq!(watch.toggle(), Some(-3));
        assert_eq!(watch.toggle(), None);
        assert!(watch.details);
        // Closing brings a row above the screen to its top.
        watch.drawn = Some(drawn(Some(-3), None));
        assert_eq!(watch.hint(), Some(true));
        assert_eq!(watch.toggle(), Some(0));
        assert!(!watch.details);
        // A click lands on the row the last frame drew, at the scroll it drew.
        watch.drawn = Some(drawn(Some(4), Some(9)));
        assert!(watch.toggle_at(9, 7));
        assert!(!watch.toggle_at(9, 8) && !watch.toggle_at(8, 7));
        // A card with no Details row has nothing to toggle and keeps its record.
        watch.drawn = Some(drawn(None, None));
        assert_eq!((watch.toggle(), watch.hint()), (None, None));
        assert!(watch.drawn.is_some());
    }
}
