//! The tasks drawer above the composer: the checklist the running turn keeps, as the nightly
//! desktop draws it in `apps/web/src/components/chat/ComposerTasksBadge.tsx`. Which checklist
//! counts follows `deriveActivePlanState` (`apps/web/src/session-logic.ts:261`) and
//! `activeComposerTasksProgress` (`apps/web/src/components/ChatView.tsx:2450-2474`). When the
//! drawer shows and when it closes follows `apps/web/src/components/chat/ChatComposer.tsx`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;
use super::{fit, row};
use crate::projection::{self, ThreadState};
use crate::transcript::{StepStatus, TaskStep, clusters, task_steps};

/// The most rows the open list takes. The GUI caps it at `min(24rem, 40dvh)`
/// (`ComposerBanner.tsx`, `Scroll`). 24rem is 384px and a step row is 25px, a 16px line with
/// `py-1` and the list's 1px gap, so 15 rows. The 40% is of the terminal's height.
const MAX_LIST_ROWS: usize = 15;
/// The bar of step segments shows for 2 to 10 steps (`MAX_TASK_SEGMENTS`).
const MAX_SEGMENTS: usize = 10;
/// The GUI shows the bar in a drawer at least 560px wide (`@min-[560px]`). Sixty columns
/// leaves the summary row room for the bar and the step.
const SEGMENTS_MIN_WIDTH: usize = 60;
/// From this width the summary row names its key.
const KEY_MIN_WIDTH: usize = 40;
/// Below this width the drawer isn't drawn.
const MIN_WIDTH: usize = 8;
/// The narrowest step text that keeps the time column beside it.
const MIN_TEXT: usize = 12;

/// The checklist of the run that owns the thread's work, as the drawer shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tasks {
    pub steps: Vec<TaskStep>,
    /// The step the summary row names: the running one, else the first pending one, else the
    /// last.
    pub current: usize,
    pub completed: usize,
}

impl Tasks {
    fn of(steps: Vec<TaskStep>) -> Option<Tasks> {
        let last = steps.len().checked_sub(1)?;
        let first = |status| steps.iter().position(|step| step.status == status);
        let current = first(StepStatus::Running)
            .or_else(|| first(StepStatus::Pending))
            .unwrap_or(last);
        let completed = steps
            .iter()
            .filter(|step| step.status == StepStatus::Completed)
            .count();
        Some(Tasks {
            steps,
            current,
            completed,
        })
    }

    pub fn total(&self) -> usize {
        self.steps.len()
    }
}

/// Run statuses that leave a run unsettled (`isLatestRunSettled`, `session-logic.ts:220`).
/// The GUI also keeps a settled run unsettled while the thread's runtime names it the active
/// run (`:233`). For a thread T3 projects, ChatView takes the run and the runtime from the same
/// projection (`ChatView.tsx:2113-2137`). There `deriveThreadRuntime` names as active only the
/// newest run preparing, starting or running (`threadExecution.ts:257`), and the activity run
/// is settled only when no run is preparing, starting, running or waiting
/// (`threadExecution.ts:101`). So the runtime never names a settled activity run, and the
/// status alone decides.
fn unsettled(status: &str) -> bool {
    matches!(
        status,
        "preparing" | "queued" | "starting" | "running" | "waiting"
    )
}

fn run_of(plan: &Value) -> Option<&str> {
    plan.get("runId").and_then(Value::as_str)
}

/// The `todo_list` the drawer shows for the thread, or None. It is the newest list the run
/// that owns the thread's work wrote, while that run hasn't settled. A thread that has never
/// run shows its newest list only when no run wrote it. Another run's list never stands in,
/// and a list with no steps shows nothing.
fn checklist(state: &ThreadState) -> Option<&Value> {
    let run = state.activity_run();
    if run.is_some_and(|run| !unsettled(projection::status(run))) {
        return None;
    }
    let run_id = run.and_then(|run| run.get("id")).and_then(Value::as_str);
    // Newest first, so the search stops at the list it wants.
    let mut lists = state
        .list("plans")
        .iter()
        .rev()
        .filter(|plan| plan.get("kind").and_then(Value::as_str) == Some("todo_list"));
    // `deriveActivePlanState` takes the run's newest list, else the thread's newest, and
    // `activeComposerTasksProgress` then drops a list that isn't the run's own. Together that
    // is the run's newest list, or with no run, the thread's newest if no run wrote it.
    let list = match run_id {
        Some(_) => lists.find(|plan| run_of(plan) == run_id),
        None => lists.next().filter(|plan| run_of(plan).is_none()),
    }?;
    let steps = list.get("steps").and_then(Value::as_array);
    steps.is_some_and(|steps| !steps.is_empty()).then_some(list)
}

/// The list for the drawer, when it may show. It needs a live watch, since the GUI hides the
/// tasks while a thread syncs (`ChatComposer.tsx:1808`) and a watch that is connecting,
/// reconnecting or closed can be behind. A waiting request hides it too: its panel takes the
/// place the GUI gives its approval and question drawer (`ChatComposer.tsx:5114-5120`).
fn shown(state: Option<&ThreadState>, live: bool) -> Option<&Value> {
    let state = state.filter(|_| live)?;
    if !state.pending_requests().is_empty() {
        return None;
    }
    checklist(state)
}

/// A duration as the nightly's `formatDuration` writes it
/// (`packages/shared/src/orchestrationTiming.ts:36`), quirks included: 999.6ms reads `1000ms`
/// and 59.6s reads `60s`.
pub fn format_duration(ms: f64) -> String {
    if !ms.is_finite() || ms < 0.0 {
        return "0ms".into();
    }
    if ms < 1_000.0 {
        return format!("{}ms", (ms.round() as u64).max(1));
    }
    if ms < 10_000.0 {
        let tenths = (ms / 100.0).round() as u64;
        return if tenths >= 100 {
            "10s".into()
        } else {
            format!("{}.{}s", tenths / 10, tenths % 10)
        };
    }
    if ms < 60_000.0 {
        return format!("{}s", (ms / 1_000.0).round() as u64);
    }
    let total = (ms / 1_000.0).round() as u64;
    [
        (total / 3_600, "h"),
        (total % 3_600 / 60, "m"),
        (total % 60, "s"),
    ]
    .iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, unit)| format!("{count}{unit}"))
    .collect::<Vec<_>>()
    .join(" ")
}

/// A step's time column: what it took once T3 has recorded it, `now` while it runs.
fn step_time(step: &TaskStep) -> String {
    match (step.duration_ms, step.status) {
        (Some(ms), _) => format_duration(ms),
        (None, StepStatus::Running) => "now".into(),
        (None, _) => String::new(),
    }
}

/// The tasks the drawer shows and what the reader has done with it. Only this window keeps
/// either, as the GUI keeps them in React state: nothing goes to T3 or the settings file.
#[derive(Debug, Default)]
pub struct Drawer {
    /// Whether the list is open under the summary row.
    pub open: bool,
    /// Rows the open list is scrolled down.
    scroll: usize,
    /// Where the last frame drew the drawer, for the mouse. Empty when it drew none, and once
    /// the tasks have gone.
    pub area: Rect,
    /// Whether the last frame's list was too long for its rows, which is when it scrolls.
    overflows: bool,
    /// The tasks on show, as of the last change to the thread or its watch.
    shown: Option<Shown>,
    /// How many times the drawer read a list's steps, and wrapped them into rows, so the tests
    /// can tell a frame that redrew from what the drawer kept.
    #[cfg(test)]
    reads: usize,
    #[cfg(test)]
    wraps: usize,
}

/// The list the drawer shows and what it made of it, so a frame that nothing changed draws
/// from these instead of reading the list and wrapping its steps again. It holds one list, and
/// its rows at one width: another list replaces it, another width rewraps it, and it goes
/// when the tasks go.
#[derive(Debug)]
struct Shown {
    /// The list as T3 sent it, to tell a change to it from a change elsewhere in the thread.
    source: Value,
    tasks: Tasks,
    /// The steps as rows, made when the list first opens at a width.
    rows: Option<Rows>,
}

/// The open list's rows at one width, as text. A frame styles the rows it shows as it copies
/// them, so the rows hold no colors and a theme needs no rewrap.
#[derive(Debug)]
struct Rows {
    width: usize,
    /// The time column with the space before it, or 0 when a row can't spare it.
    column: usize,
    rows: Vec<StepRow>,
}

/// A row of the open list: a piece of a step's text. A step's first row carries its mark and
/// its time.
#[derive(Debug)]
struct StepRow {
    step: usize,
    first: bool,
    text: String,
}

impl Drawer {
    /// Brings the drawer up to date after a change to the thread or its watch, or another
    /// thread opening. It reads the list again only when the list changed. Tasks that go away
    /// close the list and forget its scroll, as the GUI's effects do when the tasks go, a
    /// request opens or another thread opens (`ChatComposer.tsx:5615-5629`). That happens on
    /// each change rather than at the next frame, so a request that opens and resolves between
    /// two frames still closes the list.
    pub fn follow(&mut self, state: Option<&ThreadState>, live: bool) {
        let Some(list) = shown(state, live) else {
            self.hide();
            return;
        };
        let same = |kept: &Shown| kept.source == *list;
        if self.shown.as_ref().is_some_and(same) {
            return;
        }
        #[cfg(test)]
        {
            self.reads += 1;
        }
        let Some(tasks) = Tasks::of(task_steps(list)) else {
            self.hide();
            return;
        };
        self.shown = Some(Shown {
            source: list.clone(),
            tasks,
            rows: None,
        });
    }

    /// Closes the list and forgets the tasks and where the drawer was drawn, so a key or a
    /// click before the next frame doesn't reach a drawer that has gone.
    fn hide(&mut self) {
        self.close();
        self.shown = None;
        self.area = Rect::default();
    }

    /// Closes the list and forgets its scroll.
    fn close(&mut self) {
        self.open = false;
        self.scroll = 0;
        self.overflows = false;
    }

    /// Opens or closes the list. It opens at the top.
    fn toggle(&mut self) {
        let open = !self.open;
        self.close();
        self.open = open;
    }

    /// Alt+T opens or closes the list from any pane while the drawer shows. With no drawer on
    /// screen the key isn't the drawer's and goes where it went before there was one, so Esc
    /// then a quick `t`, which terminals send as Alt+T, still reaches the transcript's `t`.
    /// Alt+↑/↓ scroll a list too long to show. Returns whether the key was the drawer's.
    pub fn on_key(&mut self, key: &KeyEvent) -> bool {
        let modifiers = key.modifiers;
        if !modifiers.contains(KeyModifiers::ALT) || modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match key.code {
            KeyCode::Char('t' | 'T') if self.area.height > 0 => self.toggle(),
            KeyCode::Up if self.overflows => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down if self.overflows => self.scroll += 1,
            _ => return false,
        }
        true
    }

    /// A click on the summary row opens or closes the list, and the wheel scrolls a list too
    /// long to show. Other clicks on the drawer do nothing, so they leave the focus where it
    /// was, as the GUI's tab does. Returns None for events outside the drawer, else whether
    /// the drawer changed.
    pub fn on_mouse(&mut self, mouse: &MouseEvent) -> Option<bool> {
        let area = self.area;
        let inside = mouse.column >= area.x
            && mouse.column < area.x + area.width
            && mouse.row >= area.y
            && mouse.row < area.y + area.height;
        if !inside {
            return None;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if mouse.row == area.y => self.toggle(),
            MouseEventKind::ScrollUp if self.overflows => {
                self.scroll = self.scroll.saturating_sub(1)
            }
            MouseEventKind::ScrollDown if self.overflows => self.scroll += 1,
            _ => return Some(false),
        }
        Some(true)
    }

    /// The drawer's rows within `width` columns and `room` rows: the summary row, then the
    /// steps while the list is open. `screen` is the terminal's height, 40% of which caps the
    /// list. With no tasks the drawer draws nothing. Neither does a drawer with no room, which
    /// keeps whether its list is open for when the room comes back. The steps wrap once per
    /// list and width, and a frame copies only the rows it shows.
    pub fn lay_out(
        &mut self,
        width: usize,
        room: usize,
        screen: usize,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        self.overflows = false;
        let Some(shown) = self.shown.as_mut() else {
            return Vec::new();
        };
        if room == 0 || width < MIN_WIDTH {
            return Vec::new();
        }
        let mut lines = vec![summary(&shown.tasks, self.open, width, theme)];
        if !self.open {
            return lines;
        }
        if shown.rows.as_ref().is_none_or(|rows| rows.width != width) {
            #[cfg(test)]
            {
                self.wraps += 1;
            }
            shown.rows = Some(Rows::of(&shown.tasks, width));
        }
        let Some(rows) = shown.rows.as_ref() else {
            return lines;
        };
        let tasks = &shown.tasks;
        let line = |part: &StepRow| rows.line(part, tasks, theme);
        let total = rows.rows.len();
        let list_rows = (room - 1).min(MAX_LIST_ROWS).min(screen * 2 / 5);
        if total <= list_rows {
            self.scroll = 0;
            lines.extend(rows.rows.iter().map(line));
        } else if list_rows > 0 {
            // Too long: the list scrolls above a hint row, as a long request does.
            let visible = list_rows - 1;
            self.overflows = visible > 0;
            self.scroll = self.scroll.min(total - visible);
            let hint = if visible == 0 {
                "Make the window taller to see the tasks".to_string()
            } else {
                format!(
                    "Lines {}-{} of {total} · Alt+↑/↓ scroll",
                    self.scroll + 1,
                    self.scroll + visible
                )
            };
            let on_screen = &rows.rows[self.scroll..self.scroll + visible];
            lines.extend(on_screen.iter().map(line));
            lines.push(Line::styled(
                fit(&hint, width),
                Style::new().fg(theme.muted),
            ));
        }
        lines
    }
}

fn status_color(status: StepStatus, theme: &Theme) -> Color {
    match status {
        StepStatus::Completed => theme.emerald,
        StepStatus::Running => theme.primary,
        StepStatus::Pending => theme.border_strong,
    }
}

/// The summary row: the icon, `Tasks`, the current step, the count, the step bar when there is
/// room for it, Alt+T and the chevron, which points up while the list is closed. The count
/// turns green once every step is done. Green is the GUI's `--success`, emerald-500.
fn summary(tasks: &Tasks, open: bool, width: usize, theme: &Theme) -> Line<'static> {
    let muted = Style::new().fg(theme.muted);
    let count_color = if tasks.completed >= tasks.total() {
        theme.emerald
    } else {
        theme.muted
    };
    let count = Span::styled(
        format!("{}/{}", tasks.completed, tasks.total()),
        Style::new().fg(count_color),
    );
    let chevron = if open { " ▾" } else { " ▴" };
    let mut right = vec![count.clone()];
    if (2..=MAX_SEGMENTS).contains(&tasks.total()) && width >= SEGMENTS_MIN_WIDTH {
        let cells = MAX_SEGMENTS / tasks.total();
        right.push(Span::raw(" "));
        right.extend(tasks.steps.iter().map(|step| {
            Span::styled(
                "━".repeat(cells),
                Style::new().fg(status_color(step.status, theme)),
            )
        }));
    }
    if width >= KEY_MIN_WIDTH {
        right.push(Span::styled("  Alt+T", muted));
    }
    right.push(Span::styled(chevron, muted));
    let label = "≡ Tasks ";
    let right_width: usize = right.iter().map(|span| span.content.width()).sum();
    if label.width() + right_width > width {
        // Too narrow for the label: the icon, the count and the chevron.
        let text = fit(&format!("≡ {}{chevron}", count.content), width);
        return Line::styled(text, count.style);
    }
    let mut left = vec![Span::styled(label, muted)];
    let room = width - label.width() - right_width;
    if room > 1 {
        let step = &tasks.steps[tasks.current].text;
        // `fit` can end a column over when it cuts after a wide character. A column less then
        // keeps the space before the count.
        let mut text = fit(step, room - 1);
        if text.width() > room - 1 {
            text = fit(step, room - 2);
        }
        left.push(Span::styled(text, Style::new().fg(theme.fg)));
    }
    row(left, right, width)
}

impl Rows {
    /// Each step as rows: its text broken between words, or inside a word too long for a row as
    /// the GUI's `wrap-anywhere` breaks it, clear of the time column on the right of its first
    /// row.
    fn of(tasks: &Tasks, width: usize) -> Rows {
        let times = tasks.steps.iter().map(step_time);
        let time_width = times.map(|time| time.width()).max().unwrap_or(0);
        // The time column, when the row keeps room for some text beside it.
        let column = if time_width > 0 && width >= 2 + MIN_TEXT + 1 + time_width {
            time_width + 1
        } else {
            0
        };
        let text_width = width.saturating_sub(2 + column).max(1);
        let mut rows = Vec::new();
        for (step, item) in tasks.steps.iter().enumerate() {
            for (index, text) in wrap(&item.text, text_width).into_iter().enumerate() {
                rows.push(StepRow {
                    step,
                    first: index == 0,
                    text,
                });
            }
        }
        Rows {
            width,
            column,
            rows,
        }
    }

    /// A row as a frame draws it: a step's first row with its mark and, when the column has
    /// room, its time, and the step's other rows under its text.
    fn line(&self, part: &StepRow, tasks: &Tasks, theme: &Theme) -> Line<'static> {
        let step = &tasks.steps[part.step];
        let (mark, text_color) = match step.status {
            StepStatus::Completed => ("✓ ", theme.muted),
            StepStatus::Running => ("◉ ", theme.fg),
            StepStatus::Pending => ("○ ", theme.muted),
        };
        let lead = if part.first {
            Span::styled(mark, Style::new().fg(status_color(step.status, theme)))
        } else {
            Span::raw("  ")
        };
        let text = Span::styled(part.text.clone(), Style::new().fg(text_color));
        let time = if part.first && self.column > 0 {
            step_time(step)
        } else {
            String::new()
        };
        let right = if time.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(time, Style::new().fg(theme.muted))]
        };
        row(vec![lead, text], right, self.width)
    }
}

/// `text` in rows of at most `width` columns, broken at spaces, or inside a word too long for a
/// row as the GUI's `wrap-anywhere` breaks it. Each piece `clusters` finds is measured whole, as
/// the terminal draws it, so ⚠ with U+FE0F counts two columns, though ⚠ alone counts one. A
/// break drops its space, and a run of spaces inside a row reads as one. A compaction's summary
/// and a handoff's endpoints in the transcript wrap the same way.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        let mut used = 0;
        for word in line.split(' ').filter(|word| !word.is_empty()) {
            let pieces = clusters(word);
            let word_width: usize = pieces.iter().map(|piece| piece.width()).sum();
            if !row.is_empty() {
                if used + 1 + word_width <= width {
                    row.push(' ');
                    row.push_str(word);
                    used += 1 + word_width;
                    continue;
                }
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            for piece in pieces {
                let piece_width = piece.width();
                if used > 0 && used + piece_width > width {
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
                row.push_str(piece);
                used += piece_width;
            }
        }
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Depth;
    use serde_json::json;

    fn thread(runs: Vec<Value>, plans: Vec<Value>) -> ThreadState {
        let projection = json!({
            "thread": {"id": "t"},
            "runs": runs,
            "plans": plans,
            "runtimeRequests": [],
        });
        let snapshot = json!({"snapshotSequence": 1, "projection": projection});
        ThreadState::from_snapshot(&snapshot).expect("a snapshot")
    }

    fn run(id: &str, ordinal: u64, status: &str) -> Value {
        json!({"id": id, "ordinal": ordinal, "status": status})
    }

    /// A `todo_list` plan artifact as the nightly projects it, with `(text, status)` steps.
    fn list(id: &str, run: Option<&str>, steps: &[(&str, &str)]) -> Value {
        let steps: Vec<Value> = steps
            .iter()
            .map(|(text, status)| json!({"id": text, "text": text, "status": status}))
            .collect();
        json!({
            "id": id,
            "threadId": "t",
            "runId": run,
            "nodeId": "n",
            "kind": "todo_list",
            "status": "active",
            "steps": steps,
        })
    }

    /// Applies one live event, as the watch does.
    fn update(state: &mut ThreadState, kind: &str, payload: Value) {
        let event = json!({"type": kind, "payload": payload});
        let sequence = state.sequence + 1;
        state.apply(&json!({"kind": "event", "sequence": sequence, "event": event}));
    }

    fn tasks_of(plan: &Value) -> Tasks {
        Tasks::of(task_steps(plan)).expect("tasks")
    }

    /// The tasks the drawer would show for the thread, read as `follow` reads them.
    fn active(state: &ThreadState) -> Option<Tasks> {
        checklist(state).and_then(|list| Tasks::of(task_steps(list)))
    }

    /// A drawer showing `tasks`, as `follow` leaves it.
    fn showing(tasks: Tasks, open: bool) -> Drawer {
        let shown = Shown {
            source: Value::Null,
            tasks,
            rows: None,
        };
        Drawer {
            open,
            shown: Some(shown),
            ..Drawer::default()
        }
    }

    /// Every row of the open list at `width`, as one frame tall enough for them all draws them.
    fn step_rows(tasks: &Tasks, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let rows = Rows::of(tasks, width);
        let line = |part: &StepRow| rows.line(part, tasks, theme);
        rows.rows.iter().map(line).collect()
    }

    fn texts(tasks: Option<Tasks>) -> Vec<String> {
        let steps = tasks.expect("tasks").steps;
        steps.into_iter().map(|step| step.text).collect()
    }

    fn current(tasks: &Tasks) -> &str {
        &tasks.steps[tasks.current].text
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn the_drawer_follows_its_list_as_steps_finish_and_hides_when_the_run_ends() {
        let mut state = thread(vec![run("r1", 1, "running")], vec![]);
        // The run starts before it writes a list.
        assert_eq!(active(&state), None);
        let steps = [("Read the log", "running"), ("Patch it", "pending")];
        let plan = list("p1", Some("r1"), &steps);
        update(&mut state, "plan.updated", plan);
        let tasks = active(&state).expect("tasks");
        assert_eq!((tasks.completed, tasks.total()), (0, 2));
        assert_eq!(current(&tasks), "Read the log");

        // T3 sends the whole list again with each change, and times a step once it is done.
        let steps = [("Read the log", "completed"), ("Patch it", "running")];
        let mut plan = list("p1", Some("r1"), &steps);
        plan["steps"][0]["durationMs"] = json!(1500);
        update(&mut state, "plan.updated", plan);
        let tasks = active(&state).expect("tasks");
        assert_eq!((tasks.completed, current(&tasks)), (1, "Patch it"));
        assert_eq!(step_time(&tasks.steps[0]), "1.5s");
        assert_eq!(step_time(&tasks.steps[1]), "now");

        // Every step done while the run winds down: the count is full and names the last step.
        let steps = [("Read the log", "completed"), ("Patch it", "completed")];
        let plan = list("p1", Some("r1"), &steps);
        update(&mut state, "plan.updated", plan);
        let tasks = active(&state).expect("tasks");
        assert_eq!((tasks.completed, current(&tasks)), (2, "Patch it"));

        update(&mut state, "run.updated", run("r1", 1, "completed"));
        assert_eq!(active(&state), None);
    }

    #[test]
    fn a_new_turn_never_shows_the_last_turns_list() {
        let old = list("p1", Some("r1"), &[("Old step", "running")]);
        // r2 has written nothing yet. The GUI's plan lookup falls back to r1's list, and the
        // drawer refuses it.
        for status in ["preparing", "starting", "running", "waiting"] {
            let runs = vec![run("r1", 1, "completed"), run("r2", 2, status)];
            let state = thread(runs, vec![old.clone()]);
            assert_eq!(active(&state), None, "{status}");
        }
        // Once r2 writes one, it shows, wherever it sits among the plans.
        let new = list("p2", Some("r2"), &[("New step", "running")]);
        let runs = vec![run("r1", 1, "completed"), run("r2", 2, "running")];
        let state = thread(runs.clone(), vec![old.clone(), new.clone()]);
        assert_eq!(texts(active(&state)), ["New step"]);
        let state = thread(runs, vec![new, old]);
        assert_eq!(texts(active(&state)), ["New step"]);
    }

    #[test]
    fn a_settled_run_shows_no_tasks_even_with_steps_left() {
        let plan = list("p1", Some("r1"), &[("Half done", "running")]);
        let settled = [
            "completed",
            "interrupted",
            "failed",
            "cancelled",
            "rolled_back",
        ];
        for status in settled {
            let state = thread(vec![run("r1", 1, status)], vec![plan.clone()]);
            assert_eq!(active(&state), None, "{status}");
        }
    }

    #[test]
    fn a_queued_follow_up_leaves_the_working_runs_list_up() {
        let plans = vec![list("p1", Some("r1"), &[("Go on", "running")])];
        let runs = vec![run("r1", 1, "running"), run("r2", 2, "queued")];
        let state = thread(runs, plans.clone());
        assert_eq!(texts(active(&state)), ["Go on"]);
        // Once r1 ends, the queued run owns the thread, and it has no list yet.
        let runs = vec![run("r1", 1, "completed"), run("r2", 2, "queued")];
        let state = thread(runs, plans.clone());
        assert_eq!(active(&state), None);
        // A run held in the queue doesn't own the thread. r1 does, and it has settled.
        let mut held = run("r2", 2, "queued");
        held["queueHeld"] = json!(true);
        let state = thread(vec![run("r1", 1, "completed"), held], plans);
        assert_eq!(active(&state), None);
    }

    #[test]
    fn the_runs_newest_list_counts_and_an_empty_one_shows_nothing() {
        let runs = vec![run("r1", 1, "running")];
        let first = list("p1", Some("r1"), &[("First try", "completed")]);
        let second = list("p2", Some("r1"), &[("Second try", "running")]);
        let emptied = list("p2", Some("r1"), &[]);
        // A proposed plan written later isn't a checklist, so it doesn't take the list's place.
        let mut proposal = list("p3", Some("r1"), &[]);
        proposal["kind"] = json!("proposed_plan");
        let plans = vec![first.clone(), second, proposal.clone()];
        let mut state = thread(runs, plans);
        assert_eq!(texts(active(&state)), ["Second try"]);
        // A newest list with no steps hides the drawer rather than bring back the older one.
        update(&mut state, "plan.updated", emptied);
        assert_eq!(active(&state), None);
    }

    #[test]
    fn a_thread_that_never_ran_shows_only_a_list_no_run_wrote() {
        let loose = list("p1", None, &[("Set up", "running")]);
        assert_eq!(texts(active(&thread(vec![], vec![loose]))), ["Set up"]);
        let orphan = list("p1", Some("gone"), &[("Set up", "running")]);
        assert_eq!(active(&thread(vec![], vec![orphan])), None);
    }

    #[test]
    fn the_summary_names_the_running_step_then_the_next_pending_one_then_the_last() {
        let current_of = |steps: &[(&str, &str)]| {
            let plan = list("p1", Some("r1"), steps);
            let state = thread(vec![run("r1", 1, "running")], vec![plan]);
            current(&active(&state).expect("tasks")).to_string()
        };
        let running = [("a", "completed"), ("b", "pending"), ("c", "running")];
        assert_eq!(current_of(&running), "c");
        let pending = [("a", "completed"), ("b", "pending"), ("c", "pending")];
        assert_eq!(current_of(&pending), "b");
        let done = [("a", "completed"), ("b", "completed")];
        assert_eq!(current_of(&done), "b");
    }

    #[test]
    fn a_request_or_a_watch_that_isnt_live_hides_the_drawer() {
        let plan = list("p1", Some("r1"), &[("Ask first", "running")]);
        let mut state = thread(vec![run("r1", 1, "waiting")], vec![plan]);
        assert!(shown(Some(&state), true).is_some());
        // Connecting, reconnecting or closed.
        assert!(shown(Some(&state), false).is_none());
        assert!(shown(None, true).is_none());
        let request = |status: &str| json!({"id": "q1", "kind": "command", "status": status});
        let pending = request("pending");
        update(&mut state, "runtime-request.updated", pending);
        assert!(shown(Some(&state), true).is_none());
        let resolved = request("resolved");
        update(&mut state, "runtime-request.updated", resolved);
        assert!(shown(Some(&state), true).is_some());
    }

    #[test]
    fn durations_read_as_the_nightly_writes_them() {
        // The nightly's own cases, from `orchestrationTiming.test.ts`.
        let cases = [
            (0.0, "1ms"),
            (250.0, "250ms"),
            (1_500.0, "1.5s"),
            (9_950.0, "10s"),
            (22_000.0, "22s"),
            (60_000.0, "1m"),
            (65_000.0, "1m 5s"),
            (119_500.0, "2m"),
            (3_599_499.0, "59m 59s"),
            (3_599_500.0, "1h"),
            (3_600_000.0, "1h"),
            (3_601_000.0, "1h 1s"),
            (3_660_000.0, "1h 1m"),
            (3_661_000.0, "1h 1m 1s"),
            (7_199_500.0, "2h"),
            (25_190_000.0, "6h 59m 50s"),
            (90_061_000.0, "25h 1m 1s"),
        ];
        for (ms, expected) in cases {
            assert_eq!(format_duration(ms), expected, "{ms}");
        }
        for ms in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(format_duration(ms), "0ms", "{ms}");
        }
        // Steps with the same text keep their own times, and a pending step has none.
        let steps = [
            ("Verify", "completed"),
            ("Verify", "completed"),
            ("Report", "pending"),
        ];
        let mut plan = list("p1", None, &steps);
        plan["steps"][0]["durationMs"] = json!(3000);
        plan["steps"][1]["durationMs"] = json!(4000);
        let times: Vec<String> = tasks_of(&plan).steps.iter().map(step_time).collect();
        assert_eq!(times, ["3.0s", "4.0s", ""]);
    }

    #[test]
    fn alt_t_is_the_drawers_only_while_it_shows_and_letters_stay_text() {
        let mut drawer = Drawer {
            area: Rect::new(2, 10, 40, 1),
            ..Drawer::default()
        };
        // Letters are the composer's text, and `t` and `p` are the transcript's.
        for c in ['t', 'T', 'p', 'a'] {
            for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
                let typed = key(KeyCode::Char(c), modifiers);
                assert!(!drawer.on_key(&typed), "{typed:?}");
            }
        }
        // Alt+A, S and D answer requests, and Alt+M, E and P open menus. Ctrl+Alt+T isn't Alt+T.
        for c in ['a', 's', 'd', 'm', 'e', 'p'] {
            let other = key(KeyCode::Char(c), KeyModifiers::ALT);
            assert!(!drawer.on_key(&other), "{other:?}");
        }
        let ctrl_alt = KeyModifiers::ALT | KeyModifiers::CONTROL;
        assert!(!drawer.on_key(&key(KeyCode::Char('t'), ctrl_alt)));
        assert!(!drawer.open);

        let alt_t = key(KeyCode::Char('t'), KeyModifiers::ALT);
        assert!(drawer.on_key(&alt_t));
        assert!(drawer.open);
        let alt_shift = KeyModifiers::ALT | KeyModifiers::SHIFT;
        assert!(drawer.on_key(&key(KeyCode::Char('T'), alt_shift)));
        assert!(!drawer.open);

        // A list that fits leaves Alt+↑/↓ to the rest of the TUI.
        assert!(!drawer.on_key(&key(KeyCode::Up, KeyModifiers::ALT)));
        assert!(!drawer.on_key(&key(KeyCode::Down, KeyModifiers::ALT)));

        // With no drawer on screen, Alt+T isn't the drawer's. It goes on as it did before the
        // drawer, so Esc then a quick `t` in the transcript still opens the tool-call rows.
        drawer.area = Rect::default();
        assert!(!drawer.on_key(&alt_t));
        assert!(!drawer.on_key(&key(KeyCode::Char('T'), alt_shift)));
        assert!(!drawer.open);
        // When the drawer comes back, Alt+T is its again.
        drawer.area = Rect::new(2, 10, 40, 1);
        assert!(drawer.on_key(&alt_t));
        assert!(drawer.open);
    }

    #[test]
    fn a_click_on_the_summary_row_toggles_and_other_clicks_on_the_drawer_do_nothing() {
        let mouse = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let click = MouseEventKind::Down(MouseButton::Left);
        let mut drawer = Drawer {
            area: Rect::new(2, 10, 40, 4),
            ..Drawer::default()
        };
        assert_eq!(drawer.on_mouse(&mouse(click, 20, 10)), Some(true));
        assert!(drawer.open);
        // A step row, or the wheel over a list that fits.
        assert_eq!(drawer.on_mouse(&mouse(click, 20, 12)), Some(false));
        let wheel = MouseEventKind::ScrollDown;
        assert_eq!(drawer.on_mouse(&mouse(wheel, 20, 12)), Some(false));
        assert!(drawer.open);
        // Outside the drawer, the event is someone else's.
        assert_eq!(drawer.on_mouse(&mouse(click, 1, 10)), None);
        assert_eq!(drawer.on_mouse(&mouse(click, 20, 14)), None);
        assert_eq!(drawer.on_mouse(&mouse(click, 20, 9)), None);
        drawer.area = Rect::default();
        assert_eq!(drawer.on_mouse(&mouse(click, 0, 0)), None);
    }

    /// r1's list of thirty one-row steps: four done, the fifth running and the rest to do.
    fn long_plan() -> Value {
        let steps: Vec<Value> = (1..=30)
            .map(|n| {
                let status = match n {
                    1..=4 => "completed",
                    5 => "running",
                    _ => "pending",
                };
                json!({"id": n, "text": format!("Step {n}"), "status": status})
            })
            .collect();
        let mut plan = list("p1", Some("r1"), &[]);
        plan["steps"] = json!(steps);
        plan
    }

    fn long_list() -> Tasks {
        tasks_of(&long_plan())
    }

    #[test]
    fn a_long_list_scrolls_under_a_hint_and_keeps_to_its_rows() {
        let theme = Theme::new(Depth::TrueColor);
        let mut drawer = showing(long_list(), false);
        // Closed, the drawer is its summary row, whatever the room.
        let rows = text(&drawer.lay_out(50, 40, 60, &theme));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].starts_with("≡ Tasks Step 5"), "{}", rows[0]);
        assert!(rows[0].ends_with("4/30  Alt+T ▴"), "{}", rows[0]);

        drawer.open = true;
        let rows = text(&drawer.lay_out(50, 40, 60, &theme));
        // The summary, 14 steps and the hint: the list keeps to the GUI's 15 rows.
        assert_eq!(rows.len(), 16);
        assert!(rows[0].ends_with("4/30  Alt+T ▾"), "{}", rows[0]);
        assert!(rows[1].starts_with("✓ Step 1"), "{}", rows[1]);
        assert!(rows[5].starts_with("◉ Step 5"), "{}", rows[5]);
        assert!(rows[5].ends_with("now"), "{}", rows[5]);
        assert!(rows[6].starts_with("○ Step 6"), "{}", rows[6]);
        assert_eq!(rows[15], "Lines 1-14 of 30 · Alt+↑/↓ scroll");

        // Alt+↓ and the wheel scroll it, and scrolling past the end stops at the last step.
        drawer.area = Rect::new(0, 0, 50, 16);
        let alt_down = key(KeyCode::Down, KeyModifiers::ALT);
        assert!(drawer.on_key(&alt_down));
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 3,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(drawer.on_mouse(&wheel), Some(true));
        let rows = text(&drawer.lay_out(50, 40, 60, &theme));
        assert!(rows[1].starts_with("✓ Step 3"), "{}", rows[1]);
        for _ in 0..40 {
            drawer.on_key(&alt_down);
        }
        let rows = text(&drawer.lay_out(50, 40, 60, &theme));
        assert!(rows[1].starts_with("○ Step 17"), "{}", rows[1]);
        assert!(rows[14].starts_with("○ Step 30"), "{}", rows[14]);
        assert_eq!(rows[15], "Lines 17-30 of 30 · Alt+↑/↓ scroll");

        // A 20-row terminal gives the list 40% of its height: 8 rows, the hint among them. The
        // scroll stays where it was while it still fits.
        let rows = text(&drawer.lay_out(50, 40, 20, &theme));
        assert_eq!(rows.len(), 9);
        assert!(rows[8].starts_with("Lines 17-23 of 30"), "{}", rows[8]);
        // Less room than that: the summary and as much of the list as fits.
        let rows = text(&drawer.lay_out(50, 4, 60, &theme));
        assert_eq!(rows.len(), 4);
        assert!(rows[3].starts_with("Lines 17-18 of 30"), "{}", rows[3]);
        let rows = text(&drawer.lay_out(50, 1, 60, &theme));
        assert_eq!(rows.len(), 1);
        assert!(!drawer.on_key(&alt_down));
        // No room at all draws nothing, and the list stays open for when the room comes back.
        assert!(drawer.lay_out(50, 0, 60, &theme).is_empty());
        assert!(drawer.open);

        // When the tasks go away the drawer closes and draws nothing.
        drawer.follow(None, true);
        assert!(!drawer.open);
        assert!(drawer.lay_out(50, 40, 60, &theme).is_empty());
    }

    #[test]
    fn a_frame_nothing_changed_draws_from_what_the_drawer_kept() {
        let theme = Theme::new(Depth::TrueColor);
        let plan = long_plan();
        let mut state = thread(vec![run("r1", 1, "running")], vec![plan.clone()]);
        let mut drawer = Drawer::default();
        drawer.follow(Some(&state), true);
        assert_eq!((drawer.reads, drawer.wraps), (1, 0));
        // Closed, the drawer draws its summary without wrapping a step.
        assert_eq!(text(&drawer.lay_out(50, 40, 60, &theme)).len(), 1);
        assert_eq!(drawer.wraps, 0);
        drawer.area = Rect::new(0, 0, 50, 1);
        assert!(drawer.on_key(&key(KeyCode::Char('t'), KeyModifiers::ALT)));
        let opened = text(&drawer.lay_out(50, 40, 60, &theme));
        assert_eq!((drawer.reads, drawer.wraps), (1, 1));

        // Frames that typing or the clock cause, a reply streaming in and T3 sending the same
        // list again all draw from what the drawer kept.
        for _ in 0..5 {
            assert_eq!(text(&drawer.lay_out(50, 40, 60, &theme)), opened);
        }
        let reply = json!({"id": "a1", "type": "assistant_message", "text": "On it"});
        update(&mut state, "turn-item.updated", reply);
        drawer.follow(Some(&state), true);
        update(&mut state, "plan.updated", plan.clone());
        drawer.follow(Some(&state), true);
        assert_eq!(text(&drawer.lay_out(50, 40, 60, &theme)), opened);
        assert_eq!((drawer.reads, drawer.wraps), (1, 1));

        // Scrolling moves which kept rows the frame copies.
        drawer.area = Rect::new(0, 0, 50, 16);
        assert!(drawer.on_key(&key(KeyCode::Down, KeyModifiers::ALT)));
        let scrolled = text(&drawer.lay_out(50, 40, 60, &theme));
        assert!(scrolled[1].starts_with("✓ Step 2"), "{}", scrolled[1]);
        assert_eq!(scrolled[15], "Lines 2-15 of 30 · Alt+↑/↓ scroll");
        assert_eq!(drawer.wraps, 1);

        // The kept rows hold no colors, so another theme restyles them without a rewrap.
        let indexed = Theme::new(Depth::Indexed);
        let restyled = drawer.lay_out(50, 40, 60, &indexed);
        assert_eq!(text(&restyled), scrolled);
        assert_eq!(restyled[1].spans[0].style.fg, Some(indexed.emerald));
        assert_eq!(drawer.wraps, 1);

        // Another width wraps the list again, and only the last width is kept.
        drawer.lay_out(30, 40, 60, &theme);
        assert_eq!(drawer.wraps, 2);
        let shown = drawer.shown.as_ref().expect("tasks");
        assert_eq!(shown.rows.as_ref().map(|rows| rows.width), Some(30));
        assert_eq!(text(&drawer.lay_out(50, 40, 60, &theme)), scrolled);
        assert_eq!(drawer.wraps, 3);

        // A change to the list reads it again and wraps it at the next frame. The list stays
        // open where it was scrolled, with the new count, mark and time: step 5 took 2.5s
        // and step 6 runs.
        let mut changed = plan;
        changed["steps"][4]["status"] = json!("completed");
        changed["steps"][4]["durationMs"] = json!(2500);
        changed["steps"][5]["status"] = json!("running");
        update(&mut state, "plan.updated", changed);
        drawer.follow(Some(&state), true);
        assert_eq!((drawer.reads, drawer.wraps), (2, 3));
        assert!(drawer.open);
        let rows = text(&drawer.lay_out(50, 40, 60, &theme));
        assert_eq!(drawer.wraps, 4);
        assert!(rows[0].starts_with("≡ Tasks Step 6"), "{}", rows[0]);
        assert!(rows[0].ends_with("5/30  Alt+T ▾"), "{}", rows[0]);
        assert!(rows[1].starts_with("✓ Step 2"), "{}", rows[1]);
        assert!(rows[4].starts_with("✓ Step 5"), "{}", rows[4]);
        assert!(rows[4].ends_with("2.5s"), "{}", rows[4]);
        assert!(rows[5].starts_with("◉ Step 6"), "{}", rows[5]);
        assert!(rows[5].ends_with("now"), "{}", rows[5]);
        assert_eq!(rows[15], "Lines 2-15 of 30 · Alt+↑/↓ scroll");

        // The tasks going away drops what the drawer kept.
        update(&mut state, "run.updated", run("r1", 1, "completed"));
        drawer.follow(Some(&state), true);
        assert!(drawer.shown.is_none());
    }

    /// r1's long list, shown open and scrolled three rows down, as the last frame drew it.
    fn scrolled() -> (ThreadState, Drawer) {
        let theme = Theme::new(Depth::TrueColor);
        let state = thread(vec![run("r1", 1, "running")], vec![long_plan()]);
        let mut drawer = Drawer::default();
        drawer.follow(Some(&state), true);
        drawer.area = Rect::new(0, 0, 50, 1);
        assert!(drawer.on_key(&key(KeyCode::Char('t'), KeyModifiers::ALT)));
        drawer.lay_out(50, 40, 60, &theme);
        for _ in 0..3 {
            assert!(drawer.on_key(&key(KeyCode::Down, KeyModifiers::ALT)));
        }
        let rows = text(&drawer.lay_out(50, 40, 60, &theme));
        assert!(rows[1].starts_with("✓ Step 4"), "{}", rows[1]);
        drawer.area = Rect::new(0, 0, 50, 16);
        (state, drawer)
    }

    /// Checks that the next frame draws the drawer closed, and that it opens at the top.
    fn closed_at_top(drawer: &mut Drawer, case: &str) {
        let theme = Theme::new(Depth::TrueColor);
        let rows = text(&drawer.lay_out(50, 40, 60, &theme));
        assert_eq!(rows.len(), 1, "{case}");
        assert!(rows[0].ends_with("Alt+T ▴"), "{case}: {}", rows[0]);
        drawer.area = Rect::new(0, 0, 50, 1);
        assert!(drawer.on_key(&key(KeyCode::Char('t'), KeyModifiers::ALT)));
        let rows = text(&drawer.lay_out(50, 40, 60, &theme));
        assert!(rows[1].starts_with("✓ Step 1"), "{case}: {}", rows[1]);
        assert!(
            rows[15].starts_with("Lines 1-14 of 30"),
            "{case}: {}",
            rows[15]
        );
    }

    #[test]
    fn tasks_that_go_and_come_back_between_frames_still_close_the_list() {
        // Each case applies its events with no frame between them, as when they arrive within
        // one frame's 33ms. The tasks go with the first event and are back after the last.
        let request = |status: &str| json!({"id": "q1", "kind": "command", "status": status});
        let (mut state, mut drawer) = scrolled();
        update(&mut state, "runtime-request.updated", request("pending"));
        drawer.follow(Some(&state), true);
        // Until the next frame, nothing is kept, and neither Alt+T nor a click is the drawer's.
        assert!(drawer.shown.is_none());
        assert!(!drawer.on_key(&key(KeyCode::Char('t'), KeyModifiers::ALT)));
        assert!(!drawer.on_key(&key(KeyCode::Down, KeyModifiers::ALT)));
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(drawer.on_mouse(&click), None);
        update(&mut state, "runtime-request.updated", request("resolved"));
        drawer.follow(Some(&state), true);
        closed_at_top(&mut drawer, "a request");

        let (mut state, mut drawer) = scrolled();
        update(&mut state, "plan.updated", list("p1", Some("r1"), &[]));
        drawer.follow(Some(&state), true);
        update(&mut state, "plan.updated", long_plan());
        drawer.follow(Some(&state), true);
        closed_at_top(&mut drawer, "an emptied list");

        // The run settles, and the next one starts and writes its own list.
        let (mut state, mut drawer) = scrolled();
        update(&mut state, "run.updated", run("r1", 1, "completed"));
        drawer.follow(Some(&state), true);
        update(&mut state, "run.created", run("r2", 2, "running"));
        drawer.follow(Some(&state), true);
        let mut next = long_plan();
        next["id"] = json!("p2");
        next["runId"] = json!("r2");
        update(&mut state, "plan.updated", next);
        drawer.follow(Some(&state), true);
        closed_at_top(&mut drawer, "a new run");

        // The watch drops, T3 sends a snapshot on the new socket, then the watch is live.
        let (mut state, mut drawer) = scrolled();
        drawer.follow(Some(&state), false);
        let mut snapshot = json!({"kind": "snapshot", "snapshotSequence": 9});
        snapshot["projection"] = Value::Object(state.projection.clone());
        state.apply(&snapshot);
        drawer.follow(Some(&state), false);
        state.apply(&json!({"kind": "synchronized"}));
        drawer.follow(Some(&state), true);
        closed_at_top(&mut drawer, "a reconnect");

        // Another thread opens, then this one again.
        let (state, mut drawer) = scrolled();
        drawer.follow(None, false);
        drawer.follow(Some(&state), true);
        closed_at_top(&mut drawer, "another thread");
    }

    #[test]
    fn a_step_draws_without_its_control_characters() {
        let theme = Theme::new(Depth::TrueColor);
        let steps = [
            ("Clear\u{1b}[2J the screen", "running"),
            ("Copy\u{1b}]52;c;Zm9v\u{7} and \u{9b}31m color", "pending"),
            ("Fake\rReal", "pending"),
        ];
        let plan = list("p1", Some("r1"), &steps);
        let state = thread(vec![run("r1", 1, "running")], vec![plan]);
        let mut drawer = Drawer {
            open: true,
            ..Drawer::default()
        };
        drawer.follow(Some(&state), true);
        let rows = text(&drawer.lay_out(40, 40, 60, &theme));
        assert_eq!(rows.len(), 5);
        for row in &rows {
            let control = row.chars().find(|c| c.is_control());
            assert_eq!(control, None, "{row:?}");
            assert!(row.width() <= 40, "{row:?}");
        }
        assert!(
            rows[0].starts_with("≡ Tasks Clear[2J the screen"),
            "{:?}",
            rows[0]
        );
        assert_eq!(rows[2].trim_end(), "○ Copy]52;c;Zm9v and 31m color");
        assert_eq!(rows[3].trim_end(), "○ Fake");
        assert_eq!(rows[4].trim_end(), "  Real");
    }

    /// ⚠ and U+FE0F, which a terminal draws two columns wide, though ⚠ alone is one.
    const WARN: &str = "⚠\u{fe0f}";
    /// 👩‍💻, two emoji and the zero-width joiner between them, two columns wide.
    const CODER: &str = "👩\u{200d}💻";
    /// 👍 with a skin tone, two columns wide.
    const THUMBS: &str = "👍\u{1f3fd}";

    /// Steps a terminal draws at a width other than their characters' sum, or wider than one
    /// column each: emoji with a variation selector, a joiner or a skin tone, CJK with no
    /// spaces, combining accents, and CJK mixed with ASCII. The second step runs.
    fn unicode_list() -> Tasks {
        let (warnings, coders) = (WARN.repeat(10), CODER.repeat(3));
        let emoji = format!("Ship 🚀 then flag {warnings} for {coders}{THUMBS} review");
        let cjk = "修复解析器中的游标错误并在重新连接后保留会话状态";
        let accents = "Check the cafe\u{301} menu in Tie\u{302}\u{301}ng Vie\u{323}\u{302}t";
        let mixed = "检查 README 的 日本語 セクション";
        let steps = [
            (emoji.as_str(), "completed"),
            (cjk, "running"),
            (accents, "pending"),
            (mixed, "pending"),
        ];
        let mut plan = list("p1", Some("r1"), &steps);
        plan["steps"][0]["durationMs"] = json!(3_661_000);
        tasks_of(&plan)
    }

    /// `text` without its spaces, which a row break drops.
    fn squeeze(text: &str) -> String {
        text.replace(' ', "")
    }

    /// Lays out `tasks`, open, at every width up to 90 columns. No row runs past the drawer,
    /// the summary keeps a space before the `count`, and however narrow, each step's rows hold
    /// all of its text after one mark for its state.
    fn check_every_width(tasks: &Tasks, count: &str) {
        let theme = Theme::new(Depth::TrueColor);
        let expected: Vec<(String, String)> = tasks
            .steps
            .iter()
            .map(|step| {
                let mark = match step.status {
                    StepStatus::Completed => "✓ ",
                    StepStatus::Running => "◉ ",
                    StepStatus::Pending => "○ ",
                };
                (mark.to_string(), squeeze(&step.text))
            })
            .collect();
        let gap = format!(" {count}");
        for width in 0..=90 {
            let mut drawer = showing(tasks.clone(), true);
            let rows = text(&drawer.lay_out(width, 40, 60, &theme));
            if width < MIN_WIDTH {
                assert!(rows.is_empty(), "{width}");
                continue;
            }
            for row in &rows {
                assert!(row.width() <= width, "{width}: {row:?}");
            }
            assert!(rows[0].contains(&gap), "{width}: {:?}", rows[0]);
            let list = step_rows(tasks, width, &theme);
            for row in text(&list) {
                assert!(row.width() <= width, "{width}: {row:?}");
            }
            // A row that opens with a mark starts a step, and a row that opens with spaces
            // goes on with the step above it.
            let mut steps: Vec<(String, String)> = Vec::new();
            for line in &list {
                let lead: &str = &line.spans[0].content;
                let part = squeeze(&line.spans[1].content);
                if lead == "  " {
                    let (_, whole) = steps.last_mut().expect("a step above");
                    whole.push_str(&part);
                } else {
                    steps.push((lead.to_string(), part));
                }
            }
            assert_eq!(steps, expected, "{width}");
        }
    }

    #[test]
    fn every_row_keeps_inside_the_drawer_and_every_step_keeps_its_text() {
        let theme = Theme::new(Depth::TrueColor);
        let steps = [
            (
                "Read every log under /var/log/agent and note the failures",
                "completed",
            ),
            (
                "Patch src/projection_reducer_with_a_long_name.rs so the cursor stays",
                "running",
            ),
            ("Run the tests", "pending"),
        ];
        let mut plan = list("p1", Some("r1"), &steps);
        plan["steps"][0]["durationMs"] = json!(3_661_000);
        let tasks = tasks_of(&plan);
        check_every_width(&tasks, "1/3");
        check_every_width(&unicode_list(), "1/4");

        let mut drawer = showing(tasks, true);
        let rows = text(&drawer.lay_out(50, 40, 60, &theme));
        // A step's time sits on its first row, and its text wraps clear of the time column.
        assert!(rows[1].starts_with("✓ Read every log"), "{}", rows[1]);
        assert!(rows[1].ends_with("1h 1m 1s"), "{}", rows[1]);
        assert!(rows[2].starts_with("  "), "{}", rows[2]);
        assert!(!rows[2].contains("1h"), "{}", rows[2]);
        // At 8 columns the summary row keeps the icon, the count and the chevron.
        let rows = text(&drawer.lay_out(8, 40, 60, &theme));
        assert_eq!(rows[0], "≡ 1/3 ▾");

        // At 30 columns the running CJK step is cut a column short of where `fit` would cut it,
        // so a space stays before the count.
        let mut drawer = showing(unicode_list(), true);
        let rows = text(&drawer.lay_out(30, 40, 60, &theme));
        assert_eq!(rows[0], "≡ Tasks 修复解析器中的…  1/4 ▾");
        // Each ⚠️ takes two of the step's 19 columns, so nine fit on a row.
        assert_eq!(rows[1], "✓ Ship 🚀 then flag   1h 1m 1s");
        assert_eq!(rows[2].trim_end(), format!("  {}", WARN.repeat(9)));
        let joined = format!("  {WARN} for {}{THUMBS}", CODER.repeat(3));
        assert_eq!(rows[3].trim_end(), joined);
        assert!(rows[5].starts_with("◉ 修复解析器中的游标"), "{}", rows[5]);
        assert!(rows[5].ends_with("now"), "{}", rows[5]);
    }

    #[test]
    fn steps_wrap_by_the_columns_a_terminal_draws() {
        // CJK with no spaces breaks between characters, two columns each, so nine fit in 19.
        let cjk = "修复解析器中的游标错误并在重新连接后保留会话状态";
        let chars: Vec<char> = cjk.chars().collect();
        let rows: Vec<String> = chars.chunks(9).map(|row| row.iter().collect()).collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(wrap(cjk, 19), rows);
        // Ten ⚠️ take 20 columns, not the 10 their visible characters would.
        let flagged = format!("flag {} for", WARN.repeat(10));
        let rows = [
            "flag".to_string(),
            WARN.repeat(6),
            format!("{} for", WARN.repeat(4)),
        ];
        assert_eq!(wrap(&flagged, 12), rows);
        // A joined emoji and a skin tone stay whole, and accents stay on their letters.
        let team = format!("{CODER}{CODER}{CODER}{THUMBS}");
        let rows = [CODER.repeat(2), format!("{CODER}{THUMBS}")];
        assert_eq!(wrap(&team, 5), rows);
        let accented = wrap("Re\u{301}sume\u{301}", 3);
        assert_eq!(accented, ["Re\u{301}s", "ume\u{301}"]);
        // A break drops its space, and an empty step still has its row.
        assert_eq!(wrap("Run the tests", 7), ["Run the", "tests"]);
        assert_eq!(wrap("", 7), [""]);
    }

    #[test]
    fn the_step_bar_shows_for_two_to_ten_steps_in_a_wide_drawer() {
        let theme = Theme::new(Depth::TrueColor);
        let summary_of = |count: usize, width: usize| {
            let steps = vec![("Step", "completed"); count];
            let tasks = tasks_of(&list("p1", None, &steps));
            summary(&tasks, false, width, &theme)
        };
        let bar = |line: &Line| line.spans.iter().any(|span| span.content.contains('━'));
        assert!(bar(&summary_of(3, 70)));
        assert!(bar(&summary_of(10, 70)));
        assert!(!bar(&summary_of(1, 70)));
        assert!(!bar(&summary_of(11, 70)));
        assert!(!bar(&summary_of(3, 50)));

        // Every step done turns the count green.
        let done = summary_of(3, 70);
        assert_eq!(done.width(), 70);
        let count = done.spans.iter().find(|span| span.content == "3/3");
        assert_eq!(count.expect("the count").style.fg, Some(theme.emerald));
    }
}
