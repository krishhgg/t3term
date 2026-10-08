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
use textwrap::{Options, WordSeparator, WrapAlgorithm};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;
use super::{fit, row};
use crate::projection::{self, ThreadState};
use crate::transcript::{StepStatus, TaskStep, task_steps};

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

/// Run statuses that leave a run unsettled (`isLatestRunSettled`, `session-logic.ts:219`).
fn unsettled(status: &str) -> bool {
    matches!(status, "preparing" | "queued" | "starting" | "running" | "waiting")
}

fn run_of(plan: &Value) -> Option<&str> {
    plan.get("runId").and_then(Value::as_str)
}

/// The checklist the drawer shows for the thread, or None. It is the newest `todo_list` the
/// run that owns the thread's work wrote, while that run hasn't settled. A thread that has
/// never run shows its newest list only when no run wrote it. Another run's list never stands
/// in, and an empty list shows nothing.
pub fn active(state: &ThreadState) -> Option<Tasks> {
    let run = state.activity_run();
    if run.is_some_and(|run| !unsettled(projection::status(run))) {
        return None;
    }
    let run_id = run.and_then(|run| run.get("id")).and_then(Value::as_str);
    let lists: Vec<&Value> = state
        .list("plans")
        .iter()
        .filter(|plan| plan.get("kind").and_then(Value::as_str) == Some("todo_list"))
        .collect();
    // `deriveActivePlanState` takes the run's newest list, else the thread's newest, and
    // `activeComposerTasksProgress` then drops a list that isn't the run's own. Together that
    // is the run's newest list, or with no run, the thread's newest if no run wrote it.
    let list = match run_id {
        Some(_) => lists.iter().rev().find(|plan| run_of(plan) == run_id),
        None => lists.last().filter(|plan| run_of(plan).is_none()),
    }?;
    Tasks::of(task_steps(list))
}

/// The tasks for the drawer, when it may show. It needs a live watch, since the GUI hides the
/// tasks while a thread syncs (`ChatComposer.tsx:1808`) and a watch that is connecting,
/// reconnecting or closed can be behind. A waiting request hides it too: its panel takes the
/// place the GUI gives its approval and question drawer (`ChatComposer.tsx:5114-5120`).
pub fn shown(state: Option<&ThreadState>, live: bool) -> Option<Tasks> {
    let state = state.filter(|_| live)?;
    if !state.pending_requests().is_empty() {
        return None;
    }
    active(state)
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
    [(total / 3_600, "h"), (total % 3_600 / 60, "m"), (total % 60, "s")]
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

/// What the reader has done with the drawer. Only this window keeps it, as the GUI keeps it in
/// React state: nothing goes to T3 or the settings file.
#[derive(Debug, Default)]
pub struct Drawer {
    /// Whether the list is open under the summary row.
    pub open: bool,
    /// Rows the open list is scrolled down.
    scroll: usize,
    /// Where the last frame drew the drawer, for the mouse. Empty when it drew none.
    pub area: Rect,
    /// Whether the last frame's list was too long for its rows, which is when it scrolls.
    overflows: bool,
}

impl Drawer {
    /// Closes the list and forgets its scroll, as the GUI does when the tasks go away, a
    /// request opens or another thread opens.
    pub fn close(&mut self) {
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

    /// Alt+T opens or closes the list from any pane. It does nothing while no drawer shows,
    /// and never types a letter or reaches the transcript's `t`. Alt+↑/↓ scroll a list too
    /// long to show. Returns whether the key was the drawer's.
    pub fn on_key(&mut self, key: &KeyEvent) -> bool {
        let modifiers = key.modifiers;
        if !modifiers.contains(KeyModifiers::ALT) || modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match key.code {
            KeyCode::Char('t' | 'T') => {
                if self.area.height > 0 {
                    self.toggle();
                }
            }
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
    /// list. No tasks closes the drawer and draws nothing. So does a drawer with no room, which
    /// keeps whether its list is open for when the room comes back.
    pub fn lay_out(
        &mut self,
        tasks: Option<&Tasks>,
        width: usize,
        room: usize,
        screen: usize,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        self.overflows = false;
        let Some(tasks) = tasks else {
            self.close();
            return Vec::new();
        };
        if room == 0 || width < MIN_WIDTH {
            return Vec::new();
        }
        let mut lines = vec![summary(tasks, self.open, width, theme)];
        if !self.open {
            return lines;
        }
        let list = step_rows(tasks, width, theme);
        let total = list.len();
        let rows = (room - 1).min(MAX_LIST_ROWS).min(screen * 2 / 5);
        if total <= rows {
            self.scroll = 0;
            lines.extend(list);
        } else if rows > 0 {
            // Too long: the list scrolls above a hint row, as a long request does.
            let shown = rows - 1;
            self.overflows = shown > 0;
            self.scroll = self.scroll.min(total - shown);
            let hint = if shown == 0 {
                "Make the window taller to see the tasks".to_string()
            } else {
                format!(
                    "Lines {}-{} of {total} · Alt+↑/↓ scroll",
                    self.scroll + 1,
                    self.scroll + shown
                )
            };
            lines.extend(list.into_iter().skip(self.scroll).take(shown));
            lines.push(Line::styled(fit(&hint, width), Style::new().fg(theme.muted)));
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
        left.push(Span::styled(fit(step, room - 1), Style::new().fg(theme.fg)));
    }
    row(left, right, width)
}

/// Each step as rows: its mark, its text broken between words, or inside a word too long for a
/// row as the GUI's `wrap-anywhere` breaks it, and its time on the right of its first row.
fn step_rows(tasks: &Tasks, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let times: Vec<String> = tasks.steps.iter().map(step_time).collect();
    let time_width = times.iter().map(|time| time.width()).max().unwrap_or(0);
    // The time column, when the row keeps room for some text beside it.
    let column = if time_width > 0 && width >= 2 + MIN_TEXT + 1 + time_width {
        time_width + 1
    } else {
        0
    };
    let text_width = width.saturating_sub(2 + column).max(1);
    let options = Options::new(text_width)
        .word_separator(WordSeparator::AsciiSpace)
        .wrap_algorithm(WrapAlgorithm::FirstFit);
    let mut lines = Vec::new();
    for (step, time) in tasks.steps.iter().zip(times) {
        let (mark, text_color) = match step.status {
            StepStatus::Completed => ("✓ ", theme.muted),
            StepStatus::Running => ("◉ ", theme.fg),
            StepStatus::Pending => ("○ ", theme.muted),
        };
        let mark_style = Style::new().fg(status_color(step.status, theme));
        for (index, part) in textwrap::wrap(&step.text, &options).into_iter().enumerate() {
            let lead = if index == 0 {
                Span::styled(mark, mark_style)
            } else {
                Span::raw("  ")
            };
            let left = vec![
                lead,
                Span::styled(part.into_owned(), Style::new().fg(text_color)),
            ];
            let right = if index == 0 && column > 0 && !time.is_empty() {
                vec![Span::styled(time.clone(), muted)]
            } else {
                Vec::new()
            };
            lines.push(row(left, right, width));
        }
    }
    lines
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
    fn alt_t_is_the_drawers_and_letters_stay_text() {
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

        // With no drawer on screen, Alt+T still belongs to it and does nothing.
        drawer.area = Rect::default();
        assert!(drawer.on_key(&alt_t));
        assert!(!drawer.open);
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

    /// Thirty one-row steps: four done, the fifth running and the rest to do.
    fn long_list() -> Tasks {
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
        tasks_of(&json!({ "steps": steps }))
    }

    #[test]
    fn a_long_list_scrolls_under_a_hint_and_keeps_to_its_rows() {
        let theme = Theme::new(Depth::TrueColor);
        let tasks = long_list();
        let mut drawer = Drawer::default();
        // Closed, the drawer is its summary row, whatever the room.
        let rows = text(&drawer.lay_out(Some(&tasks), 50, 40, 60, &theme));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].starts_with("≡ Tasks Step 5"), "{}", rows[0]);
        assert!(rows[0].ends_with("4/30  Alt+T ▴"), "{}", rows[0]);

        drawer.open = true;
        let rows = text(&drawer.lay_out(Some(&tasks), 50, 40, 60, &theme));
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
        let rows = text(&drawer.lay_out(Some(&tasks), 50, 40, 60, &theme));
        assert!(rows[1].starts_with("✓ Step 3"), "{}", rows[1]);
        for _ in 0..40 {
            drawer.on_key(&alt_down);
        }
        let rows = text(&drawer.lay_out(Some(&tasks), 50, 40, 60, &theme));
        assert!(rows[1].starts_with("○ Step 17"), "{}", rows[1]);
        assert!(rows[14].starts_with("○ Step 30"), "{}", rows[14]);
        assert_eq!(rows[15], "Lines 17-30 of 30 · Alt+↑/↓ scroll");

        // A 20-row terminal gives the list 40% of its height: 8 rows, the hint among them. The
        // scroll stays where it was while it still fits.
        let rows = text(&drawer.lay_out(Some(&tasks), 50, 40, 20, &theme));
        assert_eq!(rows.len(), 9);
        assert!(rows[8].starts_with("Lines 17-23 of 30"), "{}", rows[8]);
        // Less room than that: the summary and as much of the list as fits.
        let rows = text(&drawer.lay_out(Some(&tasks), 50, 4, 60, &theme));
        assert_eq!(rows.len(), 4);
        assert!(rows[3].starts_with("Lines 17-18 of 30"), "{}", rows[3]);
        let rows = text(&drawer.lay_out(Some(&tasks), 50, 1, 60, &theme));
        assert_eq!(rows.len(), 1);
        assert!(!drawer.on_key(&alt_down));
        // No room at all draws nothing, and the list stays open for when the room comes back.
        assert!(drawer.lay_out(Some(&tasks), 50, 0, 60, &theme).is_empty());
        assert!(drawer.open);

        // When the tasks go away the drawer closes, so new tasks start closed and at the top.
        assert!(drawer.lay_out(None, 50, 40, 60, &theme).is_empty());
        assert!(!drawer.open);
        drawer.toggle();
        let rows = text(&drawer.lay_out(Some(&tasks), 50, 40, 60, &theme));
        assert!(rows[1].starts_with("✓ Step 1"), "{}", rows[1]);
    }

    #[test]
    fn every_row_keeps_inside_the_drawer_at_any_width() {
        let theme = Theme::new(Depth::TrueColor);
        let steps = [
            ("Read every log under /var/log/agent and note the failures", "completed"),
            ("Patch src/projection_reducer_with_a_long_name.rs so the cursor stays", "running"),
            ("Run the tests", "pending"),
        ];
        let mut plan = list("p1", Some("r1"), &steps);
        plan["steps"][0]["durationMs"] = json!(3_661_000);
        let tasks = tasks_of(&plan);
        for width in 0..=90 {
            let mut drawer = Drawer {
                open: true,
                ..Drawer::default()
            };
            let rows = text(&drawer.lay_out(Some(&tasks), width, 40, 60, &theme));
            if width < MIN_WIDTH {
                assert!(rows.is_empty(), "{width}");
                continue;
            }
            for row in &rows {
                assert!(row.width() <= width, "{width}: {row:?}");
            }
            // However narrow, each step keeps its rows, and only its first row has a mark.
            let list = text(&step_rows(&tasks, width, &theme));
            for row in &list {
                assert!(row.width() <= width, "{width}: {row:?}");
            }
            let marked = list.iter().filter(|row| !row.starts_with(' ')).count();
            assert_eq!(marked, 3, "{width}");
        }
        let mut drawer = Drawer {
            open: true,
            ..Drawer::default()
        };
        let rows = text(&drawer.lay_out(Some(&tasks), 50, 40, 60, &theme));
        // A step's time sits on its first row, and its text wraps clear of the time column.
        assert!(rows[1].starts_with("✓ Read every log"), "{}", rows[1]);
        assert!(rows[1].ends_with("1h 1m 1s"), "{}", rows[1]);
        assert!(rows[2].starts_with("  "), "{}", rows[2]);
        assert!(!rows[2].contains("1h"), "{}", rows[2]);
        // At 8 columns the summary row keeps the icon, the count and the chevron.
        let rows = text(&drawer.lay_out(Some(&tasks), 8, 40, 60, &theme));
        assert_eq!(rows[0], "≡ 1/3 ▾");
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
