//! The sidebar: which threads it shows, the shelf each one sits on, the order inside a shelf,
//! and how the list draws.
//!
//! The rules are those of T3 nightly v0.0.46-nightly.20261007.2787 (commit f570bd2):
//!
//! - Visibility is `filterSidebarV2VisibleThreads` in `apps/web/src/components/Sidebar.logic.ts`.
//! - The shelf is `resolveSidebarThreadSection` in the same file, fed by the partition in
//!   `apps/web/src/components/Sidebar.tsx`.
//! - The order inside each shelf comes from `packages/client-runtime/src/state/threadSort.ts`.
//! - Snooze is `effectiveSnoozed` in `packages/client-runtime/src/state/threadSettled.ts`.
//!
//! Field names are the shell's (`OrchestrationV2ThreadShell` in
//! `packages/contracts/src/orchestrationV2.ts`), read the way `presentThreadShell` in
//! `packages/client-runtime/src/state/models.ts` hands them to the GUI.
//!
//! A snooze ends at a wall-clock time and no server event marks it, so the sidebar reports its
//! next wake and the event loop sets one timer for it.

use std::cmp::Reverse;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::theme::{self, Theme, monogram, parse_iso_ms, relative_time};
use super::{fit, row, spinner_frame, str_of, with_bg};
use crate::projection::{ShellState, is_active_status, is_terminal_status, status};

/// The lifecycle features a server supports, from its environment descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub settlement: bool,
    pub snooze: bool,
}

impl Capabilities {
    /// Both keys are optional in `packages/contracts/src/environment.ts`. A server that leaves
    /// one out predates the feature, so only `true` turns it on.
    pub fn of(capabilities: &Value) -> Capabilities {
        Capabilities {
            settlement: capabilities["threadSettlement"] == true,
            snooze: capabilities["threadSnooze"] == true,
        }
    }
}

/// A sidebar shelf, in the order the list shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shelf {
    Pinned,
    Active,
    Snoozed,
    Settled,
}

impl Shelf {
    fn label(self) -> &'static str {
        match self {
            Shelf::Pinned => "Pinned",
            Shelf::Active => "Active",
            Shelf::Snoozed => "Snoozed",
            Shelf::Settled => "Settled",
        }
    }
}

/// `resolveSidebarThreadSection`: a snooze wins until it wakes, then settlement wins over a
/// stale pin.
fn shelf_for(snoozed: bool, settled: bool, pinned: bool) -> Shelf {
    if snoozed {
        Shelf::Snoozed
    } else if settled {
        Shelf::Settled
    } else if pinned {
        Shelf::Pinned
    } else {
        Shelf::Active
    }
}

/// The shelf a visible thread sits on at `now`. Snooze and settlement count only on a server
/// that supports them, and a pin always counts. A thread is settled when its override says so.
/// A `settledAt` stamp alone is history, not the thread's state.
fn shelf(thread: &Value, capabilities: Capabilities, now: i64) -> Shelf {
    shelf_for(
        capabilities.snooze && effective_snoozed(thread, now),
        capabilities.settlement && thread["settledOverride"] == "settled",
        !thread["pinnedAt"].is_null(),
    )
}

/// `filterSidebarV2VisibleThreads`: archived threads and subagents stay out of the sidebar,
/// since a subagent shows under its parent. A fork is an ordinary thread and stays.
fn visible(thread: &Value) -> bool {
    thread["archivedAt"].is_null() && thread["lineage"]["relationshipToParent"] != "subagent"
}

// ---- timestamps ----

/// Milliseconds for a timestamp in the form T3 writes, `2026-10-08T00:06:52.769Z`. Anything
/// else gives `None`, as `Date.parse` gives `NaN`, so a malformed value never hides a thread.
fn instant(value: &Value) -> Option<i64> {
    let text = value.as_str()?;
    let bytes = text.as_bytes();
    let (head, tail) = (bytes.get(..19)?, &bytes[19..]);
    let shape = b"dddd-dd-ddTdd:dd:dd";
    let fits = head.iter().zip(shape).all(|(&byte, &want)| {
        if want == b'd' {
            byte.is_ascii_digit()
        } else {
            byte == want
        }
    });
    let ends = match tail {
        [b'Z'] => true,
        [b'.', digits @ .., b'Z'] => !digits.is_empty() && digits.iter().all(u8::is_ascii_digit),
        _ => false,
    };
    if !fits || !ends {
        return None;
    }
    let two = |from: usize| text[from..from + 2].parse::<u32>().unwrap_or(u32::MAX);
    let (month, day, hour, minute, second) = (two(5), two(8), two(11), two(14), two(17));
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    parse_iso_ms(text)
}

/// `Date.parse(a) > Date.parse(b)`, which is false when either doesn't parse.
fn later(a: &Value, b: &Value) -> bool {
    matches!((instant(a), instant(b)), (Some(a), Some(b)) if a > b)
}

static NULL: Value = Value::Null;

// ---- snooze ----

/// The latest run as `presentThreadShell` gives it to the GUI.
struct LatestRun<'a> {
    completed: bool,
    requested_at: &'a Value,
    started_at: &'a Value,
    completed_at: &'a Value,
}

/// None before the first run. An idle thread's run counts as completed, and when the server
/// leaves out the completion time, a finished run takes `updatedAt` for it.
fn latest_run(thread: &Value) -> Option<LatestRun<'_>> {
    if thread["latestRunId"].is_null() {
        return None;
    }
    let state = status(thread);
    let completed_at = match thread.get("latestRunCompletedAt") {
        Some(value) => value,
        None if state == "idle" || is_terminal_status(state) => &thread["updatedAt"],
        None => &NULL,
    };
    Some(LatestRun {
        completed: state == "idle" || state == "completed",
        requested_at: &thread["latestRunRequestedAt"],
        started_at: &thread["latestRunStartedAt"],
        completed_at,
    })
}

/// The runtime status `shellRuntime` gives the GUI: idle while background work that wakes the
/// agent is pending, unless the run failed, else the activity run's status or the thread's own.
/// None for a thread with no run and no provider thread.
fn runtime_status(thread: &Value) -> Option<&str> {
    let state = status(thread);
    // `backgroundWorkHoldsCompletion` in `packages/shared/src/orchestrationV2PendingBackgroundWork.ts`:
    // every kind holds but a command, including kinds this build doesn't know.
    let park = state != "failed"
        && thread["pendingBackgroundTasks"]
            .as_array()
            .is_some_and(|tasks| tasks.iter().any(|task| task["kind"] != "command"));
    if thread["latestRunId"].is_null() && thread["activeProviderThreadId"].is_null() && !park {
        return None;
    }
    Some(if park {
        "idle"
    } else {
        thread["activityRunStatus"].as_str().unwrap_or(state)
    })
}

/// `threadRaisedHandWhileSnoozed`: the agent is waiting on the user, it failed after the
/// snooze began, or a run completed after the snooze began.
fn raised_hand(thread: &Value) -> bool {
    // `hasPendingApprovals` or `hasPendingUserInput`: any request but an auth refresh.
    let request = &thread["pendingRuntimeRequest"];
    if !request.is_null() && request["kind"] != "auth_refresh" {
        return true;
    }
    // A thread snoozed while already failed stays snoozed. The runtime's `updatedAt` is the
    // thread's.
    let snoozed_at = &thread["snoozedAt"];
    if matches!(runtime_status(thread), Some("error" | "failed"))
        && (snoozed_at.is_null() || later(&thread["updatedAt"], snoozed_at))
    {
        return true;
    }
    !snoozed_at.is_null()
        && latest_run(thread)
            .is_some_and(|run| run.completed && later(run.completed_at, snoozed_at))
}

/// `effectiveSnoozed`: off the active list until the wake time, unless the thread raised its
/// hand first. A wake time that has passed or doesn't parse leaves the thread where it was.
fn effective_snoozed(thread: &Value, now: i64) -> bool {
    instant(&thread["snoozedUntil"]).is_some_and(|wake| wake > now) && !raised_hand(thread)
}

// ---- order ----

fn id(thread: &Value) -> &str {
    str_of(thread, "id")
}

/// `sortPinnedThreadsByOrderKey`: threads the user arranged, by their keys, then the rest
/// newest created first. Ties go by id.
fn sort_pinned(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| match t["pinOrderKey"].as_str() {
        Some(key) => (0, key, Reverse(0), id(t)),
        None => (1, "", Reverse(instant(&t["createdAt"]).unwrap_or(0)), id(t)),
    });
}

/// `sortActiveThreadsByOrderKey`: new and reopened threads first, newest first, then the
/// threads the user arranged, by their keys. Ties go by id.
fn sort_active(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| match t["activeOrderKey"].as_str() {
        None => (0, "", Reverse(anchor(t)), id(t)),
        Some(key) => (1, key, Reverse(0), id(t)),
    });
}

/// `activeThreadAnchorTimestampMs`: when the thread was created, or when it last came back to
/// the active list if that is later.
fn anchor(thread: &Value) -> i64 {
    let at = |key: &str| instant(&thread[key]).unwrap_or(0);
    at("createdAt").max(at("unsettledAt"))
}

/// `sortSettledThreads`: newest first by `resolveSettledThreadTimestamp`. Ties go by id.
fn sort_settled(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| (Reverse(settled_ms(t)), id(t)));
}

/// `resolveSettledThreadTimestamp`: when the thread settled, else its latest message or run
/// time, else `updatedAt`. A thread with none of them sorts as 0.
fn settled_ms(thread: &Value) -> i64 {
    if let Some(at) = instant(&thread["settledAt"]) {
        return at;
    }
    let run = latest_run(thread);
    let run_times = run
        .iter()
        .flat_map(|run| [run.requested_at, run.started_at, run.completed_at]);
    std::iter::once(&thread["latestUserMessageAt"])
        .chain(run_times)
        .filter_map(instant)
        .max()
        .or_else(|| instant(&thread["updatedAt"]))
        .unwrap_or(0)
}

/// The visible threads on each shelf, in the order the GUI lists them.
#[derive(Default)]
struct Shelves<'a> {
    pinned: Vec<&'a Value>,
    active: Vec<&'a Value>,
    snoozed: Vec<&'a Value>,
    settled: Vec<&'a Value>,
}

/// The partition in `Sidebar.tsx`, less the Working shelf, a beta setting that is off by
/// default.
fn shelves(threads: &[Value], capabilities: Capabilities, now: i64) -> Shelves<'_> {
    let mut shelves = Shelves::default();
    for thread in threads.iter().filter(|t| visible(t)) {
        let list = match shelf(thread, capabilities, now) {
            Shelf::Pinned => &mut shelves.pinned,
            Shelf::Active => &mut shelves.active,
            Shelf::Snoozed => &mut shelves.snoozed,
            Shelf::Settled => &mut shelves.settled,
        };
        list.push(thread);
    }
    sort_pinned(&mut shelves.pinned);
    sort_active(&mut shelves.active);
    // Soonest wake first. Every wake here parses, or the thread wouldn't be snoozed.
    shelves
        .snoozed
        .sort_by_cached_key(|&t| instant(&t["snoozedUntil"]).unwrap_or(0));
    sort_settled(&mut shelves.settled);
    shelves
}

// ---- rows ----

#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// A shelf's name over its cards. Every heading but the first has a blank line above it.
    Heading {
        shelf: Shelf,
        gap: bool,
    },
    Thread(String),
}

impl Row {
    /// Rows on screen: a card is three lines and a gap, a heading one line and its gap.
    fn height(&self) -> usize {
        match self {
            Row::Heading { gap, .. } => 1 + usize::from(*gap),
            Row::Thread(_) => 4,
        }
    }

    /// Whether two rows show the same thing, a heading's gap aside.
    fn same(&self, other: &Row) -> bool {
        match (self, other) {
            (Row::Heading { shelf: a, .. }, Row::Heading { shelf: b, .. }) => a == b,
            (Row::Thread(a), Row::Thread(b)) => a == b,
            _ => false,
        }
    }
}

/// What the sidebar lists and where the reader is in it.
#[derive(Default)]
pub struct Sidebar {
    rows: Vec<Row>,
    selected: usize,
    /// The first row drawn.
    offset: usize,
    /// Settled threads stay behind the `Settled (N)` footer until it is opened.
    pub show_settled: bool,
    settled_count: usize,
    /// When the soonest snooze ends, in milliseconds. No server event marks it, so the event
    /// loop wakes for it.
    pub next_wake: Option<i64>,
    // Last drawn geometry, for mouse hit-testing.
    pub list: Rect,
    pub footer: Rect,
    /// The screen rows each drawn card covers, top inclusive and bottom exclusive, with its
    /// thread.
    cards: Vec<(u16, u16, String)>,
}

impl Sidebar {
    /// Lays the shelves out as rows: a heading over each shelf that has cards, then the cards.
    /// Settled cards show only while the shelf is open, except the open thread's, which never
    /// hides there (`renderedSettledThreads` in `Sidebar.tsx`). The highlight follows its
    /// thread, and the top row stays on top unless the list was scrolled to its start.
    pub fn rebuild(
        &mut self,
        threads: &[Value],
        capabilities: Capabilities,
        open_id: Option<&str>,
        now: i64,
    ) {
        let shelves = shelves(threads, capabilities, now);
        self.settled_count = shelves.settled.len();
        self.next_wake = shelves
            .snoozed
            .first()
            .and_then(|t| instant(&t["snoozedUntil"]));
        let settled = if self.show_settled {
            shelves.settled
        } else {
            shelves
                .settled
                .into_iter()
                .filter(|t| open_id == Some(id(t)))
                .collect()
        };
        let selected = self.selected_thread_id().map(str::to_string);
        // At the start of the list, new threads come in above the reader, as in the GUI.
        let top = self
            .rows
            .get(self.offset)
            .filter(|_| self.offset > 0)
            .cloned();
        let was = self.selected;
        self.rows.clear();
        for (shelf, list) in [
            (Shelf::Pinned, &shelves.pinned),
            (Shelf::Active, &shelves.active),
            (Shelf::Snoozed, &shelves.snoozed),
            (Shelf::Settled, &settled),
        ] {
            if list.is_empty() {
                continue;
            }
            let gap = !self.rows.is_empty();
            self.rows.push(Row::Heading { shelf, gap });
            self.rows
                .extend(list.iter().map(|t| Row::Thread(id(t).to_string())));
        }
        // When the highlighted thread has gone, the highlight takes the next card down, or
        // the last.
        let is_card = |row: &Row| matches!(row, Row::Thread(_));
        self.selected = selected
            .and_then(|selected| {
                self.rows
                    .iter()
                    .position(|row| matches!(row, Row::Thread(thread) if *thread == selected))
            })
            .or_else(|| {
                self.rows
                    .iter()
                    .skip(was)
                    .position(is_card)
                    .map(|index| index + was)
            })
            .or_else(|| self.rows.iter().rposition(is_card))
            .unwrap_or(0);
        self.offset = match top {
            Some(top) => self
                .rows
                .iter()
                .position(|row| row.same(&top))
                .unwrap_or(self.offset.min(self.rows.len().saturating_sub(1))),
            None => 0,
        };
    }

    pub fn selected_thread_id(&self) -> Option<&str> {
        match self.rows.get(self.selected) {
            Some(Row::Thread(id)) => Some(id),
            _ => None,
        }
    }

    /// Moves the highlight `delta` cards, past headings. It stops at either end.
    pub fn move_selection(&mut self, delta: isize) {
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

    /// Highlights a thread's card. False when the list has no card for it.
    pub fn select(&mut self, id: &str) -> bool {
        let found = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Thread(thread) if thread == id));
        if let Some(index) = found {
            self.selected = index;
        }
        found.is_some()
    }

    /// The thread whose card the last frame drew on screen row `y`. It reads what was drawn
    /// rather than the current rows, so a click lands on the card the reader saw even when the
    /// list has changed since.
    pub fn thread_at(&self, y: u16) -> Option<&str> {
        self.cards
            .iter()
            .find(|(top, bottom, _)| (*top..*bottom).contains(&y))
            .map(|(_, _, id)| id.as_str())
    }

    /// Scrolls so the highlighted card fits in `height` rows, with its shelf's heading when
    /// that fits too, and so rows above fill any room left below the last card.
    fn scroll_into_view(&mut self, height: usize) {
        let Some(last) = self.rows.len().checked_sub(1) else {
            self.offset = 0;
            return;
        };
        let span = |rows: &[Row]| rows.iter().map(Row::height).sum::<usize>();
        let selected = self.selected.min(last);
        self.offset = self.offset.min(selected);
        while self.offset < selected && span(&self.rows[self.offset..=selected]) > height {
            self.offset += 1;
        }
        if self.offset > 0
            && matches!(self.rows[self.offset - 1], Row::Heading { .. })
            && span(&self.rows[self.offset - 1..=selected]) <= height
        {
            self.offset -= 1;
        }
        while self.offset > 0 && span(&self.rows[self.offset - 1..]) <= height {
            self.offset -= 1;
        }
    }

    /// What an empty list says, after the GUI's empty state. When the list is empty only
    /// because the Settled shelf is closed, it says that, since the footer still counts the
    /// threads behind it.
    fn empty_hint(&self, shell: Option<&ShellState>) -> &'static str {
        match shell {
            None => "Loading threads…",
            Some(_) if self.settled_count > 0 => "All threads are settled",
            Some(shell) if shell.projects.is_empty() => "No projects yet",
            Some(_) => "No threads yet",
        }
    }

    /// Draws the wordmark, the shelves and the Settled footer, keeping the highlight in view.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, view: &View) {
        let t = view.theme;
        frame.render_widget(Block::new().style(Style::new().bg(t.sidebar_bg)), area);
        self.cards.clear();
        if area.width < 6 || area.height < 4 {
            // Nothing is drawn, so nothing there should take clicks.
            self.list = Rect::default();
            self.footer = Rect::default();
            return;
        }
        // A one-column strip stands in for the GUI's 1px border.
        frame.render_widget(
            Block::new().style(Style::new().bg(t.sidebar_border)),
            Rect::new(area.right() - 1, area.y, 1, area.height),
        );
        let inner = Rect::new(area.x + 1, area.y, area.width - 3, area.height);
        let width = inner.width as usize;

        let wordmark = row(
            vec![
                Span::styled(
                    "T3",
                    Style::new().fg(t.sidebar_fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" Code", Style::new().fg(t.sidebar_muted)),
            ],
            vec![Span::styled("●", Style::new().fg(view.dot))],
            width,
        );
        frame.render_widget(
            Paragraph::new(wordmark),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );

        // The shelf footer sits on the last row, as in the GUI.
        let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        self.footer = footer;
        let chevron = if self.show_settled { "⌄" } else { "›" };
        let label = format!("Settled ({})", self.settled_count);
        let footer_line = Line::from(vec![
            Span::styled(format!("{label} "), Style::new().fg(t.sidebar_muted)),
            Span::styled(
                "─".repeat(width.saturating_sub(label.width() + 3)),
                Style::new().fg(t.sidebar_border),
            ),
            Span::styled(format!(" {chevron}"), Style::new().fg(t.sidebar_muted)),
        ]);
        frame.render_widget(Paragraph::new(footer_line), footer);

        let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 3);
        self.list = list;
        let height = list.height as usize;
        self.scroll_into_view(height);
        let mut lines: Vec<Line> = Vec::with_capacity(height);
        for (index, row) in self.rows.iter().enumerate().skip(self.offset) {
            if lines.len() >= height {
                break;
            }
            match row {
                Row::Heading { shelf, gap } => {
                    if *gap {
                        lines.push(Line::default());
                    }
                    lines.push(heading(*shelf, width, t));
                }
                Row::Thread(id) => {
                    let top = list.y + lines.len() as u16;
                    self.cards
                        .push((top, (top + 3).min(list.bottom()), id.clone()));
                    let is_open = view.open_id == Some(id.as_str());
                    let cursor = index == self.selected;
                    let bg = if is_open {
                        Some(t.row_active)
                    } else if cursor {
                        Some(t.row_selected)
                    } else {
                        None
                    };
                    lines.extend(view.thread_card(id, width, bg, cursor));
                    lines.push(Line::default());
                }
            }
        }
        lines.truncate(height);
        if lines.is_empty() {
            lines.push(Line::styled(
                self.empty_hint(view.shell),
                Style::new().fg(t.sidebar_muted),
            ));
        }
        frame.render_widget(Paragraph::new(lines), list);
    }
}

/// A shelf's name and a rule out to the cards' right edge. Snoozed takes the GUI's info tone.
fn heading(shelf: Shelf, width: usize, t: &Theme) -> Line<'static> {
    let label = shelf.label();
    let color = if shelf == Shelf::Snoozed {
        t.info_fg
    } else {
        t.sidebar_muted
    };
    Line::from(vec![
        Span::styled(format!("{label} "), Style::new().fg(color)),
        Span::styled(
            "─".repeat(width.saturating_sub(label.width() + 2)),
            Style::new().fg(t.sidebar_border),
        ),
    ])
}

/// A thread from the shell, by id.
pub fn find_thread<'a>(shell: &'a ShellState, id: &str) -> Option<&'a Value> {
    shell.threads.iter().find(|t| str_of(t, "id") == id)
}

/// A project's title, or `Project` when the shell doesn't have it.
pub fn project_title(shell: Option<&ShellState>, project_id: &str) -> String {
    shell
        .and_then(|s| s.projects.iter().find(|p| str_of(p, "id") == project_id))
        .map(|p| str_of(p, "title").to_string())
        .unwrap_or_else(|| "Project".into())
}

/// What the sidebar needs from the rest of the app to draw.
pub struct View<'a> {
    pub theme: &'a Theme,
    pub shell: Option<&'a ShellState>,
    pub open_id: Option<&'a str>,
    /// Whether the sidebar has the keyboard, which colors the highlight's bar.
    pub focused: bool,
    /// Wall-clock time of the frame, in milliseconds.
    pub now: i64,
    /// The connection dot's color.
    pub dot: Color,
}

impl View<'_> {
    /// Three lines like the GUI's thread card: project and status, title, branch and provider.
    fn thread_card(
        &self,
        id: &str,
        width: usize,
        bg: Option<Color>,
        cursor: bool,
    ) -> Vec<Line<'static>> {
        let t = self.theme;
        let thread = self.shell.and_then(|shell| find_thread(shell, id));
        let title = thread
            .map(|t| str_of(t, "title"))
            .filter(|s| !s.is_empty())
            .unwrap_or("Untitled")
            .to_string();
        let project = thread
            .map(|t| project_title(self.shell, str_of(t, "projectId")))
            .unwrap_or_default();
        let badge = monogram(&project);
        let (status_label, status_color) = self.thread_status(thread);
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
            let color = if self.focused {
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
    fn thread_status(&self, thread: Option<&Value>) -> (String, Color) {
        let t = self.theme;
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
                format!(
                    "{} {}",
                    spinner_frame(self.now),
                    theme::working_label(since)
                ),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::{Buffer, Cell};
    use serde_json::json;

    use crate::tui::theme::Depth;

    const ALL: Capabilities = Capabilities {
        settlement: true,
        snooze: true,
    };

    /// The tests' clock: noon on 2026-10-08.
    fn now() -> i64 {
        parse_iso_ms("2026-10-08T12:00:00.000Z").unwrap()
    }

    fn ms(iso: &str) -> i64 {
        parse_iso_ms(iso).unwrap()
    }

    /// An idle top-level thread that has never run, with every field the sidebar reads.
    fn thread(id: &str) -> Value {
        json!({
            "id": id,
            "projectId": "p1",
            "title": format!("Thread {id}"),
            "branch": null,
            "modelSelection": {"instanceId": "codex"},
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
            "latestRunId": null,
            "activeProviderThreadId": null,
            "status": "idle",
            "pendingRuntimeRequest": null,
            "latestUserMessageAt": null,
            "createdAt": "2026-10-01T00:00:00.000Z",
            "updatedAt": "2026-10-01T00:00:00.000Z",
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
        })
    }

    fn with(mut thread: Value, fields: Value) -> Value {
        for (key, value) in fields.as_object().unwrap() {
            thread[key.as_str()] = value.clone();
        }
        thread
    }

    fn ids<'a>(threads: &[&'a Value]) -> Vec<&'a str> {
        threads.iter().map(|&t| id(t)).collect()
    }

    fn shell(threads: Vec<Value>) -> ShellState {
        ShellState {
            sequence: 1,
            projects: vec![json!({"id": "p1", "title": "Demo"})],
            threads,
            synchronized: true,
        }
    }

    fn heading_row(shelf: Shelf, gap: bool) -> Row {
        Row::Heading { shelf, gap }
    }

    fn card(id: &str) -> Row {
        Row::Thread(id.to_string())
    }

    /// Snoozed for an hour from 11:00, so asleep at noon.
    fn snoozed(id: &str) -> Value {
        with(
            thread(id),
            json!({"snoozedUntil": "2026-10-08T13:00:00.000Z", "snoozedAt": "2026-10-08T11:00:00.000Z"}),
        )
    }

    #[test]
    fn a_snooze_outranks_settlement_which_outranks_a_pin() {
        assert_eq!(shelf_for(true, true, true), Shelf::Snoozed);
        assert_eq!(shelf_for(false, true, true), Shelf::Settled);
        assert_eq!(shelf_for(false, false, true), Shelf::Pinned);
        assert_eq!(shelf_for(false, false, false), Shelf::Active);

        let everything = with(
            snoozed("t"),
            json!({"settledOverride": "settled", "pinnedAt": "2026-10-02T00:00:00.000Z"}),
        );
        assert_eq!(shelf(&everything, ALL, now()), Shelf::Snoozed);
        let woken = with(
            everything,
            json!({"snoozedUntil": "2026-10-08T11:30:00.000Z"}),
        );
        assert_eq!(shelf(&woken, ALL, now()), Shelf::Settled);
    }

    #[test]
    fn capabilities_gate_snooze_and_settlement_but_never_a_pin() {
        let everything = with(
            snoozed("t"),
            json!({"settledOverride": "settled", "pinnedAt": "2026-10-02T00:00:00.000Z"}),
        );
        let none = Capabilities::default();
        let settlement = Capabilities {
            settlement: true,
            snooze: false,
        };
        let snooze = Capabilities {
            settlement: false,
            snooze: true,
        };
        assert_eq!(shelf(&everything, none, now()), Shelf::Pinned);
        assert_eq!(shelf(&everything, settlement, now()), Shelf::Settled);
        assert_eq!(shelf(&everything, snooze, now()), Shelf::Snoozed);

        assert_eq!(
            Capabilities::of(&json!({"threadSettlement": true, "threadSnooze": "yes"})),
            settlement
        );
        assert_eq!(Capabilities::of(&Value::Null), none);
    }

    #[test]
    fn a_settled_at_stamp_alone_does_not_settle_a_thread() {
        let stamped = with(
            thread("t"),
            json!({"settledAt": "2026-10-05T00:00:00.000Z"}),
        );
        assert_eq!(shelf(&stamped, ALL, now()), Shelf::Active);
        let reopened = with(stamped, json!({"settledOverride": "active"}));
        assert_eq!(shelf(&reopened, ALL, now()), Shelf::Active);
    }

    #[test]
    fn pinned_threads_follow_their_keys_then_newest_created() {
        let pinned = |id: &str, fields: Value| {
            with(
                with(thread(id), json!({"pinnedAt": "2026-10-02T00:00:00.000Z"})),
                fields,
            )
        };
        let threads = vec![
            pinned("n1", json!({"createdAt": "2026-10-03T00:00:00.000Z"})),
            pinned("k1", json!({"pinOrderKey": "m"})),
            pinned("n3", json!({"createdAt": "not a time"})),
            pinned("k3", json!({"pinOrderKey": "c"})),
            pinned("n2", json!({"createdAt": "2026-10-05T00:00:00.000Z"})),
            pinned("k2", json!({"pinOrderKey": "c"})),
        ];
        // Capabilities don't change the pinned order.
        for capabilities in [ALL, Capabilities::default()] {
            let shelves = shelves(&threads, capabilities, now());
            assert_eq!(ids(&shelves.pinned), ["k2", "k3", "k1", "n2", "n1", "n3"]);
        }
    }

    #[test]
    fn active_threads_lead_with_new_and_reopened_then_arranged() {
        let threads = vec![
            with(
                thread("a1"),
                json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
            ),
            with(thread("k1"), json!({"activeOrderKey": "b"})),
            with(
                thread("a2"),
                json!({"createdAt": "2026-10-01T00:00:00.000Z", "unsettledAt": "2026-10-06T00:00:00.000Z"}),
            ),
            with(
                thread("a4"),
                json!({"createdAt": "2026-10-04T00:00:00.000Z"}),
            ),
            with(thread("k2"), json!({"activeOrderKey": "a"})),
            with(
                thread("a3"),
                json!({"createdAt": "2026-10-04T00:00:00.000Z"}),
            ),
        ];
        let shelves = shelves(&threads, ALL, now());
        assert_eq!(ids(&shelves.active), ["a2", "a3", "a4", "a1", "k2", "k1"]);
    }

    #[test]
    fn settled_threads_sort_by_when_they_settled_with_fallbacks() {
        let settled = |id: &str, fields: Value| {
            with(
                with(thread(id), json!({"settledOverride": "settled"})),
                fields,
            )
        };
        let threads = vec![
            settled(
                "s3",
                json!({"settledAt": null, "updatedAt": "2026-10-04T00:00:00.000Z"}),
            ),
            settled("s5", json!({"settledAt": "2026-10-05T00:00:00.000Z"})),
            settled(
                "s2",
                json!({
                    "settledAt": "soon",
                    "latestUserMessageAt": "2026-10-03T00:00:00.000Z",
                    "latestRunId": "r",
                    "latestRunCompletedAt": "2026-10-06T00:00:00.000Z",
                }),
            ),
            settled("s4", json!({"updatedAt": "bad", "createdAt": "bad"})),
            settled("s1", json!({"settledAt": "2026-10-05T00:00:00.000Z"})),
        ];
        let shelves = shelves(&threads, ALL, now());
        assert_eq!(ids(&shelves.settled), ["s2", "s1", "s5", "s3", "s4"]);
        assert_eq!(settled_ms(&threads[2]), ms("2026-10-06T00:00:00.000Z"));
        assert_eq!(settled_ms(&threads[3]), 0);
    }

    #[test]
    fn timestamps_parse_only_in_the_form_t3_writes() {
        let at = |text: &str| instant(&json!(text));
        assert_eq!(
            at("2026-10-08T12:00:00.000Z"),
            Some(ms("2026-10-08T12:00:00.000Z"))
        );
        assert_eq!(at("2026-10-08T12:00:00Z"), Some(now()));
        assert_eq!(at("2026-10-08T12:00:00.5Z"), Some(now() + 500));
        for bad in [
            "",
            "tomorrow",
            "2026-10-08",
            "2026-10-08 12:00:00Z",
            "2026-10-08T12:00:00",
            "2026-10-08T12:00:00.Z",
            "2026-10-08T12:00:00+02:00",
            "2026-13-08T12:00:00Z",
            "2026-10-32T12:00:00Z",
            "2026-10-08T24:00:00Z",
            "2026-10-08T12:60:00Z",
        ] {
            assert_eq!(at(bad), None, "{bad}");
        }
        assert_eq!(instant(&json!(12345)), None);
        assert_eq!(instant(&Value::Null), None);
    }

    #[test]
    fn a_snooze_ends_at_its_wake_time() {
        let thread = with(
            snoozed("t"),
            json!({"snoozedUntil": "2026-10-08T12:00:30.000Z"}),
        );
        let wake = ms("2026-10-08T12:00:30.000Z");
        assert!(effective_snoozed(&thread, now()));
        assert!(effective_snoozed(&thread, wake - 1));
        assert!(!effective_snoozed(&thread, wake));
        assert_eq!(shelf(&thread, ALL, wake), Shelf::Active);
    }

    #[test]
    fn a_malformed_or_missing_wake_time_never_hides_a_thread() {
        for wake in [
            json!("tomorrow"),
            json!("2026-13-01T00:00:00Z"),
            json!("2026-10-08 13:00:00Z"),
            json!(12345),
            Value::Null,
        ] {
            let thread = with(snoozed("t"), json!({"snoozedUntil": wake.clone()}));
            assert!(!effective_snoozed(&thread, now()), "{wake}");
            assert_eq!(shelf(&thread, ALL, now()), Shelf::Active, "{wake}");
        }
        let mut missing = snoozed("t");
        missing.as_object_mut().unwrap().remove("snoozedUntil");
        assert!(!effective_snoozed(&missing, now()));
    }

    #[test]
    fn a_raised_hand_wakes_a_snooze_early() {
        let woke = |fields: Value| !effective_snoozed(&with(snoozed("t"), fields), now());
        assert!(!woke(json!({})));

        // The agent waits on the user.
        assert!(woke(json!({"pendingRuntimeRequest": {"kind": "approval"}})));
        assert!(woke(
            json!({"pendingRuntimeRequest": {"kind": "user_input"}})
        ));
        assert!(!woke(
            json!({"pendingRuntimeRequest": {"kind": "auth_refresh"}})
        ));

        // A failure counts when it is newer than the snooze.
        let failed =
            |updated: &str| json!({"latestRunId": "r", "status": "failed", "updatedAt": updated});
        assert!(woke(failed("2026-10-08T11:30:00.000Z")));
        assert!(!woke(failed("2026-10-08T10:30:00.000Z")));
        assert!(woke(with(
            failed("2026-10-08T10:30:00.000Z"),
            json!({"snoozedAt": null})
        )));
        // Without a run or a provider thread there is no runtime to fail.
        assert!(!woke(
            json!({"status": "failed", "updatedAt": "2026-10-08T11:30:00.000Z"})
        ));
        // A live activity run outranks the thread's failed status.
        assert!(!woke(with(
            failed("2026-10-08T11:30:00.000Z"),
            json!({"activityRunStatus": "running"})
        )));

        // A run that completed after the snooze began.
        let completed =
            |at: &str| json!({"latestRunId": "r", "status": "idle", "latestRunCompletedAt": at});
        assert!(woke(completed("2026-10-08T11:30:00.000Z")));
        assert!(!woke(completed("2026-10-08T10:30:00.000Z")));
        assert!(!woke(with(
            completed("2026-10-08T11:30:00.000Z"),
            json!({"status": "running"})
        )));
        // A server that leaves out the completion time has it read from `updatedAt`.
        assert!(woke(json!({
            "latestRunId": "r",
            "status": "completed",
            "updatedAt": "2026-10-08T11:30:00.000Z",
        })));
        // Without `snoozedAt`, a completion can't be placed after the snooze.
        assert!(!woke(with(
            completed("2026-10-08T11:30:00.000Z"),
            json!({"snoozedAt": null})
        )));
    }

    #[test]
    fn the_soonest_wake_comes_first_and_sets_the_timer() {
        let wake = |id: &str, at: &str| with(snoozed(id), json!({"snoozedUntil": at}));
        let threads = vec![
            wake("z1", "2026-10-08T14:00:00.000Z"),
            wake("z2", "2026-10-08T12:30:00.000Z"),
            wake("z3", "2026-10-08T13:00:00.000Z"),
        ];
        assert_eq!(
            ids(&shelves(&threads, ALL, now()).snoozed),
            ["z2", "z3", "z1"]
        );

        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(sidebar.next_wake, Some(ms("2026-10-08T12:30:00.000Z")));
        sidebar.rebuild(&threads, ALL, None, ms("2026-10-08T12:30:00.000Z"));
        assert_eq!(sidebar.next_wake, Some(ms("2026-10-08T13:00:00.000Z")));
        // Without snooze support nothing is snoozed, so nothing needs a timer.
        sidebar.rebuild(&threads, Capabilities::default(), None, now());
        assert_eq!(sidebar.next_wake, None);
    }

    #[test]
    fn a_snooze_wakes_without_a_server_event() {
        let threads = vec![
            with(
                snoozed("z"),
                json!({"snoozedUntil": "2026-10-08T12:00:30.000Z"}),
            ),
            thread("a"),
        ];
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(
            sidebar.rows,
            [
                heading_row(Shelf::Active, false),
                card("a"),
                heading_row(Shelf::Snoozed, true),
                card("z"),
            ]
        );
        // The same shell, read again when the timer fires.
        sidebar.rebuild(&threads, ALL, None, ms("2026-10-08T12:00:30.050Z"));
        assert_eq!(
            sidebar.rows,
            [heading_row(Shelf::Active, false), card("a"), card("z")]
        );
        assert_eq!(sidebar.next_wake, None);
    }

    #[test]
    fn archived_threads_and_subagents_hide_but_forks_stay() {
        let archived = with(
            thread("archived"),
            json!({"archivedAt": "2026-10-05T00:00:00.000Z"}),
        );
        let subagent = with(
            thread("subagent"),
            json!({"lineage": {"parentThreadId": "a", "relationshipToParent": "subagent", "rootThreadId": "a"}}),
        );
        let fork = with(
            thread("fork"),
            json!({"lineage": {"parentThreadId": "a", "relationshipToParent": "fork", "rootThreadId": "a"}}),
        );
        assert!(!visible(&archived));
        assert!(!visible(&subagent));
        assert!(visible(&fork));
        let threads = vec![thread("a"), archived, subagent, fork];
        let shelves = shelves(&threads, ALL, now());
        let mut shown = ids(&shelves.active);
        shown.sort();
        assert_eq!(shown, ["a", "fork"]);
    }

    #[test]
    fn a_heading_leads_each_shelf_that_has_cards() {
        let threads = vec![
            with(thread("s"), json!({"settledOverride": "settled"})),
            snoozed("z"),
            thread("a"),
            with(thread("p"), json!({"pinnedAt": "2026-10-02T00:00:00.000Z"})),
        ];
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(
            sidebar.rows,
            [
                heading_row(Shelf::Pinned, false),
                card("p"),
                heading_row(Shelf::Active, true),
                card("a"),
                heading_row(Shelf::Snoozed, true),
                card("z"),
            ]
        );
        assert_eq!(sidebar.settled_count, 1);
        sidebar.show_settled = true;
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(
            sidebar.rows[6..],
            [heading_row(Shelf::Settled, true), card("s")]
        );

        sidebar.rebuild(&[thread("a")], ALL, None, now());
        assert_eq!(sidebar.rows, [heading_row(Shelf::Active, false), card("a")]);
        sidebar.rebuild(&[], ALL, None, now());
        assert!(sidebar.rows.is_empty());
        assert_eq!(sidebar.selected_thread_id(), None);
    }

    #[test]
    fn the_open_thread_stays_listed_on_a_closed_settled_shelf() {
        let threads = vec![
            thread("a"),
            with(thread("s1"), json!({"settledOverride": "settled"})),
            with(thread("s2"), json!({"settledOverride": "settled"})),
        ];
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, Some("s2"), now());
        assert_eq!(
            sidebar.rows,
            [
                heading_row(Shelf::Active, false),
                card("a"),
                heading_row(Shelf::Settled, true),
                card("s2"),
            ]
        );
        assert_eq!(sidebar.settled_count, 2);
    }

    #[test]
    fn the_highlight_follows_its_thread_when_rows_move() {
        let a = with(
            thread("a"),
            json!({"createdAt": "2026-10-03T00:00:00.000Z"}),
        );
        let b = with(
            thread("b"),
            json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
        );
        let c = with(
            thread("c"),
            json!({"createdAt": "2026-10-01T00:00:00.000Z"}),
        );
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&[a.clone(), b.clone(), c.clone()], ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("a"));
        assert!(sidebar.select("b"));
        assert!(!sidebar.select("missing"));

        // Pinning `c` adds two rows above `b`: the Pinned heading and its card.
        let pinned = with(c, json!({"pinnedAt": "2026-10-08T11:00:00.000Z"}));
        sidebar.rebuild(&[a.clone(), b, pinned.clone()], ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("b"));
        assert_eq!(sidebar.selected, 4);

        // When `b` goes, the highlight takes the last card, since none is below.
        sidebar.rebuild(&[a, pinned], ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("a"));
    }

    #[test]
    fn the_highlight_takes_the_next_card_when_its_thread_goes() {
        let threads: Vec<Value> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(i, id)| {
                with(
                    thread(id),
                    json!({"createdAt": format!("2026-10-0{}T00:00:00.000Z", 3 - i)}),
                )
            })
            .collect();
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert!(sidebar.select("b"));
        sidebar.rebuild(&[threads[0].clone(), threads[2].clone()], ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("c"));
    }

    #[test]
    fn navigation_skips_headings_and_stops_at_the_ends() {
        let threads = vec![
            with(thread("p"), json!({"pinnedAt": "2026-10-02T00:00:00.000Z"})),
            with(
                thread("a1"),
                json!({"createdAt": "2026-10-03T00:00:00.000Z"}),
            ),
            with(
                thread("a2"),
                json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
            ),
        ];
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("p"));
        sidebar.move_selection(-1);
        assert_eq!(sidebar.selected_thread_id(), Some("p"));
        sidebar.move_selection(1);
        assert_eq!(sidebar.selected_thread_id(), Some("a1"));
        sidebar.move_selection(1);
        sidebar.move_selection(1);
        assert_eq!(sidebar.selected_thread_id(), Some("a2"));
        sidebar.move_selection(-1);
        sidebar.move_selection(-1);
        assert_eq!(sidebar.selected_thread_id(), Some("p"));
    }

    // ---- drawing ----

    fn render(
        sidebar: &mut Sidebar,
        shell: Option<&ShellState>,
        open: Option<&str>,
        size: (u16, u16),
    ) -> Buffer {
        let theme = Theme::new(Depth::TrueColor);
        let view = View {
            theme: &theme,
            shell,
            open_id: open,
            focused: true,
            now: now(),
            dot: theme.success,
        };
        let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
        let frame = terminal
            .draw(|frame| {
                let area = frame.area();
                sidebar.draw(frame, area, &view);
            })
            .unwrap();
        frame.buffer.clone()
    }

    /// Each screen row as plain text.
    fn text(buffer: &Buffer) -> Vec<String> {
        let area = buffer.area;
        (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    /// The first screen row that shows `needle`.
    fn row_of(lines: &[String], needle: &str) -> u16 {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no row shows {needle:?} in {lines:#?}")) as u16
    }

    #[test]
    fn the_renderer_draws_a_heading_over_each_shelf() {
        let shell = shell(vec![
            with(thread("p"), json!({"pinnedAt": "2026-10-02T00:00:00.000Z"})),
            thread("a"),
            snoozed("z"),
            with(thread("s"), json!({"settledOverride": "settled"})),
        ]);
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&shell.threads, ALL, None, now());
        let buffer = render(&mut sidebar, Some(&shell), None, (30, 32));
        let lines = text(&buffer);
        let pinned = row_of(&lines, "Pinned ─");
        let active = row_of(&lines, "Active ─");
        let snoozed = row_of(&lines, "Snoozed ─");
        assert_eq!(pinned, 2, "the list starts under the wordmark");
        assert!(pinned < row_of(&lines, "Thread p"));
        assert!(row_of(&lines, "Thread p") < active);
        assert!(active < row_of(&lines, "Thread a"));
        assert!(row_of(&lines, "Thread a") < snoozed);
        assert!(snoozed < row_of(&lines, "Thread z"));
        // The settled card stays behind the footer, which counts it.
        assert!(!lines.iter().any(|line| line.contains("Thread s")));
        assert!(lines[31].contains("Settled (1)"));

        // Headings start one column in from the sidebar's edge, like the cards' bar.
        let theme = Theme::new(Depth::TrueColor);
        assert_eq!(buffer[(1, snoozed)].symbol(), "S");
        assert_eq!(buffer[(1, snoozed)].fg, theme.info_fg);
        assert_eq!(buffer[(1, active)].fg, theme.sidebar_muted);
    }

    #[test]
    fn a_click_lands_on_the_card_that_was_drawn() {
        let b = with(
            thread("b"),
            json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
        );
        let c = with(
            thread("c"),
            json!({"createdAt": "2026-10-01T00:00:00.000Z"}),
        );
        let first = shell(vec![b.clone(), c.clone()]);
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&first.threads, ALL, None, now());
        let lines = text(&render(&mut sidebar, Some(&first), None, (30, 24)));
        let y = row_of(&lines, "Thread c");
        assert_eq!(sidebar.thread_at(y), Some("c"));
        assert_eq!(sidebar.thread_at(y - 1), Some("c"), "the card's first line");
        assert_eq!(sidebar.thread_at(row_of(&lines, "Active ─")), None);
        assert_eq!(sidebar.thread_at(y + 2), None, "the gap under the card");

        // A new thread arrives on top before the next frame. The click still means `c`.
        let a = with(
            thread("a"),
            json!({"createdAt": "2026-10-03T00:00:00.000Z"}),
        );
        let second = shell(vec![a, b, c]);
        sidebar.rebuild(&second.threads, ALL, None, now());
        assert_eq!(sidebar.thread_at(y), Some("c"));

        // Once redrawn, the same row shows `b`, and a click there means `b`.
        let lines = text(&render(&mut sidebar, Some(&second), None, (30, 24)));
        assert!(lines[y as usize].contains("Thread b"));
        assert_eq!(sidebar.thread_at(y), Some("b"));
    }

    #[test]
    fn scrolling_keeps_the_highlight_and_its_heading_in_view() {
        let mut threads = vec![with(
            thread("p"),
            json!({"pinnedAt": "2026-10-02T00:00:00.000Z"}),
        )];
        threads.extend((1..=6).map(|n| {
            with(
                thread(&format!("a{n}")),
                json!({"createdAt": format!("2026-10-0{}T00:00:00.000Z", 8 - n)}),
            )
        }));
        let listed = shell(threads);
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&listed.threads, ALL, None, now());
        // Fourteen rows leave eleven for the list, room for two cards and a heading.
        let size = (30, 14);
        for _ in 0..6 {
            sidebar.move_selection(1);
        }
        assert_eq!(sidebar.selected_thread_id(), Some("a6"));
        let lines = text(&render(&mut sidebar, Some(&listed), None, size));
        assert!(lines.iter().any(|line| line.contains("Thread a6")));
        assert!(!lines.iter().any(|line| line.contains("Thread p")));
        assert!(
            lines[13].contains("Settled (0)"),
            "the footer keeps the last row"
        );

        // Back up to the first active card: its heading comes into view above it.
        for _ in 0..5 {
            sidebar.move_selection(-1);
        }
        assert_eq!(sidebar.selected_thread_id(), Some("a1"));
        let lines = text(&render(&mut sidebar, Some(&listed), None, size));
        assert!(row_of(&lines, "Active ─") < row_of(&lines, "Thread a1"));
        assert!(!lines.iter().any(|line| line.contains("Thread p")));

        // A thread pinned above the view doesn't move the rows the reader is looking at.
        assert_eq!(
            sidebar.rows[sidebar.offset],
            heading_row(Shelf::Active, true)
        );
        let mut more = listed.threads.clone();
        more.push(with(
            thread("p2"),
            json!({"pinnedAt": "2026-10-03T00:00:00.000Z"}),
        ));
        sidebar.rebuild(&more, ALL, None, now());
        assert_eq!(sidebar.offset, 4);
        assert_eq!(
            sidebar.rows[sidebar.offset],
            heading_row(Shelf::Active, true)
        );
        assert_eq!(sidebar.selected_thread_id(), Some("a1"));
        let lines = text(&render(&mut sidebar, Some(&shell(more)), None, size));
        assert!(row_of(&lines, "Active ─") < row_of(&lines, "Thread a1"));
        assert!(!lines.iter().any(|line| line.contains("Thread p2")));
    }

    #[test]
    fn the_empty_list_says_why() {
        let mut sidebar = Sidebar::default();
        let hint = |sidebar: &mut Sidebar, shell: Option<&ShellState>| {
            text(&render(sidebar, shell, None, (30, 12)))[2]
                .trim()
                .to_string()
        };
        assert_eq!(hint(&mut sidebar, None), "Loading threads…");

        let mut empty = shell(Vec::new());
        assert_eq!(hint(&mut sidebar, Some(&empty)), "No threads yet");
        empty.projects.clear();
        assert_eq!(hint(&mut sidebar, Some(&empty)), "No projects yet");

        let hidden = shell(vec![with(
            thread("archived"),
            json!({"archivedAt": "2026-10-05T00:00:00.000Z"}),
        )]);
        sidebar.rebuild(&hidden.threads, ALL, None, now());
        assert_eq!(hint(&mut sidebar, Some(&hidden)), "No threads yet");

        let settled = shell(vec![with(
            thread("s"),
            json!({"settledOverride": "settled"}),
        )]);
        sidebar.rebuild(&settled.threads, ALL, None, now());
        assert_eq!(
            hint(&mut sidebar, Some(&settled)),
            "All threads are settled"
        );
    }

    // ---- synthetic capture for review ----

    /// Writes the real renderer's styled output for a few invented sidebars, as ANSI text, to
    /// the directory in `T3TERM_SIDEBAR_CAPTURE`. Without it the test does nothing. Every name
    /// is made up, so the captures can be shared.
    #[test]
    fn capture_sidebar_for_review() {
        let Some(dir) = std::env::var_os("T3TERM_SIDEBAR_CAPTURE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let demo = demo_shell();
        let size = (34, 40);
        let capture =
            |name: &str, sidebar: &mut Sidebar, shell: Option<&ShellState>, open: Option<&str>| {
                let buffer = render(sidebar, shell, open, size);
                std::fs::write(dir.join(format!("sidebar-{name}.ans")), ansi(&buffer)).unwrap();
                std::fs::write(
                    dir.join(format!("sidebar-{name}.txt")),
                    text(&buffer).join("\n") + "\n",
                )
                .unwrap();
            };

        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&demo.threads, ALL, Some("t-search"), now());
        assert!(sidebar.select("t-search"));
        capture("shelves", &mut sidebar, Some(&demo), Some("t-search"));

        sidebar.show_settled = true;
        sidebar.rebuild(&demo.threads, ALL, Some("t-search"), now());
        capture("settled-open", &mut sidebar, Some(&demo), Some("t-search"));

        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&demo.threads, ALL, Some("t-notes"), now());
        assert!(sidebar.select("t-notes"));
        capture(
            "open-settled-thread",
            &mut sidebar,
            Some(&demo),
            Some("t-notes"),
        );

        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&demo.threads, Capabilities::default(), None, now());
        capture("old-server", &mut sidebar, Some(&demo), None);

        let empty = shell(Vec::new());
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&empty.threads, ALL, None, now());
        capture("empty", &mut sidebar, Some(&empty), None);
    }

    /// Invented projects and threads covering every shelf and card label.
    fn demo_shell() -> ShellState {
        let card = |id: &str, project: &str, title: &str, branch: &str, fields: Value| {
            with(
                with(
                    thread(id),
                    json!({"projectId": project, "title": title, "branch": branch}),
                ),
                fields,
            )
        };
        ShellState {
            sequence: 1,
            projects: vec![
                json!({"id": "atlas", "title": "atlas"}),
                json!({"id": "harbor", "title": "harbor"}),
            ],
            threads: vec![
                card(
                    "t-checklist",
                    "atlas",
                    "Tidy the release checklist",
                    "release/checklist",
                    json!({
                        "pinnedAt": "2026-10-05T00:00:00.000Z",
                        "pinOrderKey": "m",
                        "latestUserMessageAt": "2026-10-08T09:00:00.000Z",
                        "modelSelection": {"instanceId": "claudeAgent"},
                    }),
                ),
                card(
                    "t-search",
                    "harbor",
                    "Speed up the search index",
                    "perf/search-index",
                    json!({
                        "createdAt": "2026-10-08T08:00:00.000Z",
                        "latestRunId": "r1",
                        "status": "running",
                        "latestRunStartedAt": "2026-10-08T11:57:00.000Z",
                    }),
                ),
                card(
                    "t-upload",
                    "atlas",
                    "Fix the flaky upload test",
                    "fix/upload-test",
                    json!({
                        "createdAt": "2026-10-07T00:00:00.000Z",
                        "pendingRuntimeRequest": {"kind": "approval"},
                        "modelSelection": {"instanceId": "claudeAgent"},
                    }),
                ),
                card(
                    "t-guide",
                    "harbor",
                    "Write the onboarding guide",
                    "docs/onboarding",
                    json!({
                        "createdAt": "2026-10-06T00:00:00.000Z",
                        "latestRunId": "r2",
                        "status": "failed",
                    }),
                ),
                card(
                    "t-charts",
                    "atlas",
                    "Try a dark theme for charts",
                    "",
                    json!({
                        "createdAt": "2026-10-04T00:00:00.000Z",
                        "snoozedUntil": "2026-10-09T09:00:00.000Z",
                        "snoozedAt": "2026-10-08T10:00:00.000Z",
                        "latestUserMessageAt": "2026-10-08T10:00:00.000Z",
                    }),
                ),
                card(
                    "t-notes",
                    "harbor",
                    "Draft the changelog",
                    "docs/changelog",
                    json!({
                        "settledOverride": "settled",
                        "settledAt": "2026-10-08T07:00:00.000Z",
                        "latestUserMessageAt": "2026-10-08T06:00:00.000Z",
                    }),
                ),
                card(
                    "t-api",
                    "atlas",
                    "Plan the API cleanup",
                    "",
                    json!({
                        "settledOverride": "settled",
                        "settledAt": "2026-10-07T07:00:00.000Z",
                        "latestUserMessageAt": "2026-10-07T06:00:00.000Z",
                    }),
                ),
                card(
                    "t-helper",
                    "atlas",
                    "Subagent: scan the logs",
                    "",
                    json!({
                        "lineage": {"parentThreadId": "t-search", "relationshipToParent": "subagent", "rootThreadId": "t-search"},
                    }),
                ),
                card(
                    "t-fork",
                    "harbor",
                    "Speed up the search index (fork)",
                    "perf/search-fork",
                    json!({
                        "createdAt": "2026-10-08T09:00:00.000Z",
                        "lineage": {"parentThreadId": "t-search", "relationshipToParent": "fork", "rootThreadId": "t-search"},
                        "latestUserMessageAt": "2026-10-08T11:00:00.000Z",
                    }),
                ),
            ],
            synchronized: true,
        }
    }

    /// A buffer as ANSI text: 24-bit or indexed colors, bold and dim, one line per row.
    fn ansi(buffer: &Buffer) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            let mut skip = 0;
            for x in area.left()..area.right() {
                let cell = &buffer[(x, y)];
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                out.push_str(&sgr(cell));
                out.push_str(cell.symbol());
                skip = cell.symbol().width().saturating_sub(1);
            }
            out.push_str("\x1b[0m\n");
        }
        out
    }

    fn sgr(cell: &Cell) -> String {
        let mut codes = vec!["0".to_string()];
        if cell.modifier.contains(Modifier::BOLD) {
            codes.push("1".into());
        }
        if cell.modifier.contains(Modifier::DIM) {
            codes.push("2".into());
        }
        codes.extend(color_code(cell.fg, 38));
        codes.extend(color_code(cell.bg, 48));
        format!("\x1b[{}m", codes.join(";"))
    }

    fn color_code(color: Color, base: u8) -> Option<String> {
        match color {
            Color::Rgb(r, g, b) => Some(format!("{base};2;{r};{g};{b}")),
            Color::Indexed(index) => Some(format!("{base};5;{index}")),
            _ => None,
        }
    }
}
