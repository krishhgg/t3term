//! The thread error banner over the top of the conversation, as the nightly desktop draws
//! `ThreadErrorBanner` (`apps/web/src/components/chat/ThreadErrorBanner.tsx`). Which error it
//! shows follows `ChatView.tsx:2174-2188`: a send from this window that failed, else the error
//! `deriveThreadRuntime` (`packages/client-runtime/src/state/threadExecution.ts:224`) finds in
//! the thread's projection. Dismissing an error hides it for the rest of the session, for that
//! thread and that exact text. It changes nothing on T3, and a different error shows again.
//!
//! The banner lies over the transcript's top rows, as the desktop's overlays the timeline, so
//! an error that comes or goes doesn't move the text under it.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::tasks::wrap;
use super::theme::{Theme, parse_iso_ms};
use super::{clip, row};
use crate::projection::{Applied, ThreadState, status};
use crate::transcript::without_controls;

/// The most of an error the banner reads, in bytes. A provider failure's message is at most
/// 4,096 characters on the wire, but a provider session's `lastError` has no limit.
const MAX_BYTES: usize = 4096;
/// The widest the banner gets, in columns. The desktop's is `min(48rem, 100% - 2rem)`, and
/// 48rem is 768px, 96 columns of 8px.
const MAX_WIDTH: usize = 96;
/// Rows of text a closed banner shows, the desktop's `line-clamp-3`.
const CLAMP: usize = 3;
/// Below this many columns the banner isn't drawn. Four go to the icon and the close button.
const MIN_WIDTH: usize = 5;
/// From this many columns, with three rows to spare, the banner has a border.
const BORDER_MIN_WIDTH: usize = 12;

/// The error a thread's projection holds, as `deriveThreadRuntime` gives its `lastError` and
/// `lastErrorClass`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeError {
    pub message: String,
    /// A provider failure's class, such as `usage_limit`. None when a different session error
    /// stands in for the failure.
    pub class: Option<String>,
}

/// The thread's error, as `deriveThreadRuntime` works it out. The provider session is the
/// last one for the thread's provider instance. The run is the one that stands for the
/// thread's outcome: the run the usage limit stopped while newer runs wait behind it, else the
/// newest run not held in a queue. Its failure is the latest error of its root node. Only the
/// thread's own `turnItems` count, so an error inherited from a fork's parent never does.
///
/// A thread that has no such run and no provider thread has no runtime, so no error. The
/// desktop makes an exception for a thread that a pull request watch holds, which t3term
/// doesn't follow.
pub(super) fn derive(state: &ThreadState) -> Option<RuntimeError> {
    let thread = state.thread();
    let runs = state.list("runs");
    // One pass over the items. The rest of the work reads only the failed error items.
    let errors: Vec<&Value> = state
        .list("turnItems")
        .iter()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("error") && status(item) == "failed"
        })
        .collect();
    let instance = thread.get("providerInstanceId");
    let session_error = state
        .list("providerSessions")
        .iter()
        .rev()
        .find(|session| session.get("providerInstanceId") == instance)
        .and_then(|session| session.get("lastError"))
        .and_then(Value::as_str);
    let presented =
        usage_limit_presented(runs, &errors, session_error).or_else(|| latest_unheld(runs));
    let provider_thread = thread.get("activeProviderThreadId");
    if presented.is_none() && provider_thread.is_none_or(Value::is_null) {
        return None;
    }
    let failure = presented.and_then(|run| root_failure(run, &errors));
    let message = session_error.or_else(|| failure_message(failure))?;
    Some(RuntimeError {
        message: message.to_string(),
        class: class(failure, session_error).map(str::to_string),
    })
}

fn ordinal(run: &Value) -> u64 {
    run.get("ordinal").and_then(Value::as_u64).unwrap_or(0)
}

fn failure_message(failure: Option<&Value>) -> Option<&str> {
    failure?.get("message")?.as_str()
}

/// The failure's class, unless the session reports an error other than the failure's, as
/// `threadErrorSummary` decides (`packages/shared/src/orchestrationV2ThreadError.ts:36`).
fn class<'a>(failure: Option<&'a Value>, session_error: Option<&str>) -> Option<&'a str> {
    if session_error.is_some() && session_error != failure_message(failure) {
        return None;
    }
    failure?.get("class")?.as_str()
}

/// The `failure` of a failed run's latest root error, as `latestRootProviderFailure`
/// (`orchestrationV2ThreadError.ts:9`) picks it: an error item of this run on its root node,
/// the latest by `updatedAt`, then ordinal, then id. A failed tool, a subagent's node and
/// another run's error don't count. The ids compare as JSON values, so a missing one matches
/// only a missing one, and null only null, as JavaScript's `!==` has it.
fn root_failure<'a>(run: &Value, errors: &[&'a Value]) -> Option<&'a Value> {
    if status(run) != "failed" {
        return None;
    }
    let key = |item: &'a Value| {
        let at = item
            .get("updatedAt")
            .and_then(Value::as_str)
            .and_then(parse_iso_ms);
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        (at, ordinal(item), id)
    };
    let mut latest: Option<&Value> = None;
    for &item in errors {
        if item.get("runId") != run.get("id") || item.get("nodeId") != run.get("rootNodeId") {
            continue;
        }
        if latest.is_none_or(|latest| key(item) > key(latest)) {
            latest = Some(item);
        }
    }
    latest?.get("failure").filter(|failure| !failure.is_null())
}

/// Whether `run` ended after `other`, as `runRanAfter` (`orchestrationV2ThreadError.ts:69`)
/// has it. A run that hasn't ended counts as the latest, and runs that ended at once go by
/// ordinal. A time t3term can't read counts as not ended.
fn ran_after(run: &Value, other: &Value) -> bool {
    let end = |run: &Value| {
        run.get("completedAt")
            .and_then(Value::as_str)
            .and_then(parse_iso_ms)
            .unwrap_or(i64::MAX)
    };
    if end(run) == end(other) {
        ordinal(run) > ordinal(other)
    } else {
        end(run) > end(other)
    }
}

/// The run that started last, as `latestExecutedRun` (`orchestrationV2ThreadError.ts:51`)
/// finds it. Queued runs and runs cancelled before they started never ran.
fn latest_executed(runs: &[Value]) -> Option<&Value> {
    let mut latest: Option<&Value> = None;
    for run in runs {
        let state = status(run);
        let never_started = state == "cancelled" && run.get("startedAt") == Some(&Value::Null);
        if state == "queued" || never_started {
            continue;
        }
        if latest.is_none_or(|latest| ran_after(run, latest)) {
            latest = Some(run);
        }
    }
    latest
}

/// The run the usage limit stopped, when newer runs were queued or cancelled behind it, as
/// `usageLimitRunPresentedAsLatest` (`orchestrationV2ThreadError.ts:101`) finds it. The newest
/// run would otherwise hide the limit.
fn usage_limit_presented<'a>(
    runs: &'a [Value],
    errors: &[&Value],
    session_error: Option<&str>,
) -> Option<&'a Value> {
    let executed = latest_executed(runs)?;
    if status(executed) != "failed"
        || class(root_failure(executed, errors), session_error) != Some("usage_limit")
    {
        return None;
    }
    let limited = ordinal(executed);
    let newer = runs.iter().any(|run| ordinal(run) > limited);
    newer.then_some(executed)
}

/// The newest run not waiting in a held queue, as `latestUnheldRun`
/// (`orchestrationV2ThreadError.ts:115`) finds it.
fn latest_unheld(runs: &[Value]) -> Option<&Value> {
    let mut latest: Option<&Value> = None;
    for run in runs {
        if status(run) == "queued" && run.get("queueHeld") == Some(&Value::Bool(true)) {
            continue;
        }
        if latest.is_none_or(|latest| ordinal(run) > ordinal(latest)) {
            latest = Some(run);
        }
    }
    latest
}

/// The open thread's error, worked out again only after an event that can change it, so
/// neither a frame nor a streamed answer reads the thread for it.
#[derive(Debug, Default)]
pub(super) struct Derived {
    error: Option<RuntimeError>,
    /// Whether `error` is the projection's as of the last event. False until the first read.
    current: bool,
    /// How many times the error was worked out, so the tests can tell.
    #[cfg(test)]
    pub derives: usize,
}

impl Derived {
    /// Notes an item the thread applied. A snapshot can change anything. So can an event for
    /// a run, a provider session or the thread itself, which names the provider instance, and
    /// a turn item of type `error`. A turn item never changes its type, so any other one, such
    /// as each piece of a streamed answer, leaves the error as it was.
    pub fn note(&mut self, applied: &Applied, item: &Value) {
        let changes = match applied {
            Applied::Snapshot => true,
            Applied::Event(kind) if kind.starts_with("turn-item.") => {
                item.pointer("/event/payload/type").and_then(Value::as_str) == Some("error")
            }
            Applied::Event(kind) => ["run.", "provider-session.", "thread."]
                .iter()
                .any(|prefix| kind.starts_with(prefix)),
            _ => false,
        };
        if changes {
            self.current = false;
        }
    }

    /// The thread's error as of the last item noted.
    pub fn get(&mut self, state: Option<&ThreadState>) -> Option<&RuntimeError> {
        if !self.current {
            self.error = state.and_then(derive);
            self.current = true;
            #[cfg(test)]
            {
                self.derives += 1;
            }
        }
        self.error.as_ref()
    }
}

/// A send from this window that failed, kept for the thread it went to.
#[derive(Debug)]
struct Local {
    error: String,
    /// The id the message went out under. The error goes once the thread shows it.
    message_id: String,
}

/// The error on show and what the banner made of it.
#[derive(Debug)]
struct Shown {
    thread_id: String,
    /// The error as T3 or the send gave it, which keys its dismissal.
    raw: String,
    /// A usage limit shows as a warning rather than an error.
    warning: bool,
    /// What the banner prints: `raw`, cut to `MAX_BYTES` and without its control characters.
    text: String,
    /// Whether the reader opened the whole error.
    expanded: bool,
    /// Rows the open error is scrolled down.
    scroll: usize,
    /// `text` wrapped at one width. Another width wraps it again.
    rows: Option<Rows>,
}

#[derive(Debug)]
struct Rows {
    width: usize,
    /// The widest row, which the banner fits to.
    widest: usize,
    rows: Vec<String>,
}

/// The banner's state for the session. Only this window keeps it: nothing goes to T3 or the
/// settings file.
#[derive(Debug, Default)]
pub(super) struct Banner {
    /// Failed sends by thread, until a new send to the thread starts, the thread shows the
    /// message after all, or the reader dismisses the error.
    local: HashMap<String, Local>,
    /// Errors dismissed this session, by thread, as their raw text.
    dismissed: HashMap<String, HashSet<String>>,
    shown: Option<Shown>,
    /// Where the last frame drew the banner and its close button, for the mouse. Empty when
    /// it drew none.
    pub area: Rect,
    pub close: Rect,
    /// Whether the last frame cut the closed error short.
    clipped: bool,
    /// Whether the last frame's open error was too long for its rows, which is when it
    /// scrolls.
    overflows: bool,
    /// How many times the banner cleaned an error's text and wrapped it, so the tests can
    /// tell a frame that redrew from what the banner kept.
    #[cfg(test)]
    prepares: usize,
    #[cfg(test)]
    wraps: usize,
}

/// The error as the banner prints it: at most `MAX_BYTES` of it, cut where a character starts,
/// without its control characters and the blank space at either end. An error that was cut
/// ends with a line that says so.
fn prepare(raw: &str) -> String {
    let mut end = raw.len().min(MAX_BYTES);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    let text = without_controls(&raw[..end]).trim().to_string();
    if end < raw.len() && !text.is_empty() {
        return format!("{text}\n… the error goes on past {MAX_BYTES} bytes");
    }
    text
}

fn inside(area: Rect, mouse: &MouseEvent) -> bool {
    mouse.column >= area.x
        && mouse.column < area.x + area.width
        && mouse.row >= area.y
        && mouse.row < area.y + area.height
}

impl Banner {
    /// Keeps the error a send to `thread_id` failed with, wherever the reader is when it comes
    /// back. Until it goes, it stands for the thread in place of T3's own error, as the
    /// desktop's local error does.
    pub fn failed(&mut self, thread_id: &str, error: String, message_id: String) {
        self.local
            .insert(thread_id.to_string(), Local { error, message_id });
    }

    /// Drops the thread's last send error when a new send to it starts, as the desktop does
    /// (`ChatView.tsx:9700`).
    pub fn clear_local(&mut self, thread_id: &str) {
        self.local.remove(thread_id);
    }

    /// Drops the send error of a message the thread shows after all. A send that timed out can
    /// still reach T3.
    pub fn landed(&mut self, thread_id: &str, in_thread: impl Fn(&str) -> bool) {
        if self
            .local
            .get(thread_id)
            .is_some_and(|local| in_thread(&local.message_id))
        {
            self.local.remove(thread_id);
        }
    }

    /// Brings the banner up to date with the open thread and its error from T3. It runs after
    /// each change to either. The same error on the same thread keeps the banner as it was,
    /// open or scrolled. An empty error, a dismissed one, or one with nothing to print once
    /// its control characters are gone, such as only spaces or a terminal sequence's ESC,
    /// shows no banner.
    pub fn follow(&mut self, thread_id: Option<&str>, runtime: Option<&RuntimeError>) {
        let Some(thread_id) = thread_id else {
            self.shown = None;
            return;
        };
        // A send error stands in for T3's even when it is empty, as `??` keeps an empty string.
        let (raw, class) = match self.local.get(thread_id) {
            Some(local) => (local.error.as_str(), None),
            None => match runtime {
                Some(error) => (error.message.as_str(), error.class.as_deref()),
                None => {
                    self.shown = None;
                    return;
                }
            },
        };
        let dismissed = self
            .dismissed
            .get(thread_id)
            .is_some_and(|errors| errors.contains(raw));
        if raw.is_empty() || dismissed {
            self.shown = None;
            return;
        }
        let warning = class == Some("usage_limit");
        if let Some(shown) = self
            .shown
            .as_mut()
            .filter(|shown| shown.thread_id == thread_id && shown.raw == raw)
        {
            shown.warning = warning;
            return;
        }
        #[cfg(test)]
        {
            self.prepares += 1;
        }
        let text = prepare(raw);
        // Zero-width characters alone, such as a joiner, would draw an empty banner.
        let printable = text.split('\n').any(|line| line.width() > 0);
        self.shown = printable.then(|| Shown {
            thread_id: thread_id.to_string(),
            raw: raw.to_string(),
            warning,
            text,
            expanded: false,
            scroll: 0,
            rows: None,
        });
    }

    /// Whether the last frame drew the banner, which is when its keys and clicks apply.
    pub fn showing(&self) -> bool {
        self.shown.is_some() && self.area.height > 0
    }

    /// Hides the error on show for the rest of the session and forgets the thread's send
    /// error, as the desktop's close button does. The caller follows the thread again, which
    /// shows T3's own error if a send error was hiding it.
    pub fn dismiss(&mut self) {
        let Some(shown) = self.shown.take() else {
            return;
        };
        self.local.remove(&shown.thread_id);
        self.dismissed
            .entry(shown.thread_id)
            .or_default()
            .insert(shown.raw);
        self.area = Rect::default();
        self.close = Rect::default();
        self.clipped = false;
        self.overflows = false;
    }

    /// Opens the whole error, or closes it back to its first rows.
    fn toggle(&mut self) {
        if let Some(shown) = self.shown.as_mut() {
            shown.expanded = !shown.expanded;
            shown.scroll = 0;
        }
    }

    fn scroll(&mut self, down: bool) {
        if let Some(shown) = self.shown.as_mut() {
            shown.scroll = if down {
                shown.scroll + 1
            } else {
                shown.scroll.saturating_sub(1)
            };
        }
    }

    /// Alt+W dismisses the banner and Alt+I opens or closes the whole error, while the banner
    /// shows. Alt+↑/↓ scroll an open error too long for its rows. Returns whether the key was
    /// the banner's. With no banner on screen the keys go where they went before there was
    /// one, as Alt+T does for the tasks drawer.
    pub fn on_key(&mut self, key: &KeyEvent) -> bool {
        let modifiers = key.modifiers;
        if !self.showing()
            || !modifiers.contains(KeyModifiers::ALT)
            || modifiers.contains(KeyModifiers::CONTROL)
        {
            return false;
        }
        match key.code {
            KeyCode::Char('w' | 'W') => self.dismiss(),
            KeyCode::Char('i' | 'I') => self.toggle(),
            KeyCode::Up if self.overflows => self.scroll(false),
            KeyCode::Down if self.overflows => self.scroll(true),
            _ => return false,
        }
        true
    }

    /// A click on × dismisses the banner, and a click elsewhere on it opens or closes an error
    /// that doesn't fit. The wheel scrolls an open error too long for its rows, and otherwise
    /// the transcript under it. The banner takes its other clicks, so a row of calls or a
    /// plan under it doesn't open, and focus stays where it was. Returns None for events
    /// outside the banner and wheel events it leaves to the transcript, else whether the banner
    /// changed.
    pub fn on_mouse(&mut self, mouse: &MouseEvent) -> Option<bool> {
        if !self.showing() || !inside(self.area, mouse) {
            return None;
        }
        let expanded = self.shown.as_ref().is_some_and(|shown| shown.expanded);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if inside(self.close, mouse) => self.dismiss(),
            MouseEventKind::Down(MouseButton::Left) if self.clipped || expanded => self.toggle(),
            MouseEventKind::ScrollUp if self.overflows => self.scroll(false),
            MouseEventKind::ScrollDown if self.overflows => self.scroll(true),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => return None,
            _ => return Some(false),
        }
        Some(true)
    }

    /// Draws the banner over the top rows of `area`, the transcript's, centered and as wide as
    /// its text up to `MAX_WIDTH`. A closed banner shows at most three rows of the error, and
    /// at most half of `area`. An open one takes up to all of `area` and scrolls. With three
    /// rows and twelve columns to spare it has a border, which names its keys. With less, its
    /// rows go straight on the transcript. The error wraps once for each width.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        self.area = Rect::default();
        self.close = Rect::default();
        self.clipped = false;
        self.overflows = false;
        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        let width = area.width as usize;
        let height = area.height as usize;
        // A column of the conversation shows on each side when there is room, as the
        // desktop's `100% - 2rem` leaves.
        let room = if width >= BORDER_MIN_WIDTH + 2 {
            width - 2
        } else {
            width
        }
        .min(MAX_WIDTH);
        if room < MIN_WIDTH || height == 0 {
            return;
        }
        let limit = if shown.expanded {
            height
        } else {
            (height / 2).max(1)
        };
        let bordered = limit >= 3 && room >= BORDER_MIN_WIDTH;
        // The border, a space inside it on each side, the icon and its space, and × with a
        // space on its left. Without the border, the icon and × with one space each.
        let frame_width = if bordered { 8 } else { 4 };
        let text_width = room - frame_width;
        if shown.rows.as_ref().map(|rows| rows.width) != Some(text_width) {
            #[cfg(test)]
            {
                self.wraps += 1;
            }
            let rows = wrap(&shown.text, text_width);
            let widest = rows.iter().map(|row| row.width()).max().unwrap_or(0);
            shown.rows = Some(Rows {
                width: text_width,
                widest,
                rows,
            });
        }
        let Some(rows) = shown.rows.as_ref() else {
            return;
        };
        let total = rows.rows.len();
        let text_rows = if bordered { limit - 2 } else { limit };
        let text_rows = if shown.expanded {
            text_rows
        } else {
            text_rows.min(CLAMP)
        };
        // Never 0: the text isn't empty, so it has a row, and there is room for one.
        let visible = total.min(text_rows);
        let overflows = total > visible;
        shown.scroll = if shown.expanded {
            shown.scroll.min(total - visible)
        } else {
            0
        };
        self.clipped = !shown.expanded && overflows;
        self.overflows = shown.expanded && overflows;

        let lines_label = format!(
            "Lines {}-{} of {total}",
            shown.scroll + 1,
            shown.scroll + visible
        );
        let hints = match (shown.expanded, overflows) {
            (false, true) => vec![
                "Alt+I more · Alt+W dismiss".to_string(),
                "Alt+W dismiss".into(),
                "Alt+W".into(),
            ],
            (false, false) => vec!["Alt+W dismiss".to_string(), "Alt+W".into()],
            (true, true) => vec![
                format!("{lines_label} · Alt+↑/↓ · Alt+I less · Alt+W dismiss"),
                format!("{lines_label} · Alt+I less"),
                lines_label,
                "Alt+W".into(),
            ],
            (true, false) => vec![
                "Alt+I less · Alt+W dismiss".to_string(),
                "Alt+W dismiss".into(),
                "Alt+W".into(),
            ],
        };
        // The hint sits in the bottom border with a space on each side, between the corners.
        let hint = hints
            .into_iter()
            .filter(|_| bordered)
            .find(|hint| hint.width() + 4 <= room);
        let banner_width = (rows.widest.min(text_width) + frame_width)
            .max(hint.as_ref().map_or(0, |hint| hint.width() + 4))
            .min(room);
        let banner_height = visible + if bordered { 2 } else { 0 };
        let rect = Rect::new(
            area.x + ((width - banner_width) / 2) as u16,
            area.y,
            banner_width as u16,
            banner_height as u16,
        )
        .intersection(area);

        let (accent, icon_color) = if shown.warning {
            (theme.warning, theme.warning_fg)
        } else {
            (theme.error, theme.error_fg)
        };
        let muted = Style::new().fg(theme.muted);
        let (inner, lead, close) = if bordered {
            let inner = Rect {
                x: rect.x + 1,
                y: rect.y + 1,
                width: rect.width.saturating_sub(2),
                height: rect.height.saturating_sub(2),
            };
            (inner, " ! ", " × ")
        } else {
            (rect, "! ", " ×")
        };
        let inner_width = inner.width as usize;
        let text_room = inner_width.saturating_sub(lead.width() + close.width());
        let mut lines = Vec::with_capacity(visible);
        for (index, text) in rows.rows[shown.scroll..shown.scroll + visible]
            .iter()
            .enumerate()
        {
            // The last row of a closed error that goes on ends with …, as `line-clamp` does.
            let text = if self.clipped && index + 1 == visible {
                clip(&format!("{text}…"), text_room)
            } else {
                clip(text, text_room)
            };
            let (left, right) = if index == 0 {
                let icon = Span::styled(
                    lead.to_string(),
                    Style::new().fg(icon_color).add_modifier(Modifier::BOLD),
                );
                let close = Span::styled(close.to_string(), muted);
                (vec![icon, Span::raw(text)], vec![close])
            } else {
                (
                    vec![Span::raw(" ".repeat(lead.width())), Span::raw(text)],
                    Vec::new(),
                )
            };
            lines.push(row(left, right, inner_width));
        }

        frame.render_widget(Clear, rect);
        let style = Style::new().fg(theme.fg).bg(theme.popover);
        if bordered {
            let mut block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(accent))
                .style(style);
            if let Some(hint) = hint {
                let hint = Line::styled(format!(" {hint} "), muted).right_aligned();
                block = block.title_bottom(hint);
            }
            frame.render_widget(block, rect);
        }
        frame.render_widget(Paragraph::new(lines).style(style), inner);
        self.area = rect;
        let close_width = close.width() as u16;
        self.close = Rect::new(
            (inner.x + inner.width).saturating_sub(close_width),
            inner.y,
            close_width,
            1,
        )
        .intersection(inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Depth;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Position;
    use serde_json::json;

    // ---- which error the thread has ----

    fn run(id: &str, ordinal: u64, status: &str) -> Value {
        json!({
            "id": id,
            "ordinal": ordinal,
            "status": status,
            "rootNodeId": format!("{id}-root"),
            "startedAt": "2026-10-09T10:00:00.000Z",
            "completedAt": format!("2026-10-09T10:00:{:02}.000Z", ordinal),
        })
    }

    fn error_item(id: &str, run_id: &str, node_id: &str, message: &str, class: &str) -> Value {
        json!({
            "id": id,
            "type": "error",
            "status": "failed",
            "ordinal": 1,
            "runId": run_id,
            "nodeId": node_id,
            "updatedAt": "2026-10-09T10:00:00.000Z",
            "failure": {"class": class, "message": message},
        })
    }

    fn session(id: &str, instance: &str, last_error: Value) -> Value {
        json!({"id": id, "providerInstanceId": instance, "status": "error", "lastError": last_error})
    }

    fn thread(runs: Vec<Value>, items: Vec<Value>, sessions: Vec<Value>) -> ThreadState {
        ThreadState::from_snapshot(&json!({
            "kind": "snapshot",
            "snapshotSequence": 1,
            "projection": {
                "thread": {"id": "t", "providerInstanceId": "codex", "activeProviderThreadId": "pt"},
                "runs": runs,
                "turnItems": items,
                "providerSessions": sessions,
            },
        }))
        .unwrap()
    }

    fn error(message: &str, class: Option<&str>) -> Option<RuntimeError> {
        Some(RuntimeError {
            message: message.into(),
            class: class.map(str::to_string),
        })
    }

    #[test]
    fn a_thread_with_no_failure_has_no_error() {
        assert_eq!(derive(&thread(vec![], vec![], vec![])), None);
        let done = thread(vec![run("r1", 1, "completed")], vec![], vec![]);
        assert_eq!(derive(&done), None);
        // A completed run's error item, such as a failed attempt the run recovered from,
        // doesn't stand for the thread.
        let recovered = thread(
            vec![run("r1", 1, "completed")],
            vec![error_item("e", "r1", "r1-root", "Retrying", "transport_error")],
            vec![],
        );
        assert_eq!(derive(&recovered), None);
    }

    #[test]
    fn the_latest_runs_root_failure_is_the_error() {
        let state = thread(
            vec![run("r1", 1, "failed")],
            vec![error_item("e", "r1", "r1-root", "Provider crashed", "provider_error")],
            vec![],
        );
        assert_eq!(
            derive(&state),
            error("Provider crashed", Some("provider_error"))
        );
    }

    #[test]
    fn a_tool_a_child_an_old_run_or_an_inherited_item_never_fails_the_thread() {
        let runs = vec![run("r1", 1, "failed"), run("r2", 2, "failed")];
        // A failed tool on another node of the latest run.
        let tool = error_item("e1", "r2", "tool-node", "grep failed", "unknown");
        // A subagent's node under the root.
        let child = error_item("e2", "r2", "r2-child", "Subagent failed", "unknown");
        // The root failure of an older run.
        let old = error_item("e3", "r1", "r1-root", "Old failure", "unknown");
        // An item still running or completed.
        let mut running = error_item("e4", "r2", "r2-root", "Still going", "unknown");
        running["status"] = json!("running");
        // A different type on the root node.
        let mut message = error_item("e5", "r2", "r2-root", "Not an error", "unknown");
        message["type"] = json!("assistant_message");
        let items = vec![tool, child, old, running, message];
        let state = thread(runs.clone(), items, vec![]);
        assert_eq!(derive(&state), None);

        // A fork's parent failed with the same run and node ids, but its item comes only in
        // visibleTurnItems, which the error never reads.
        let mut inherited = thread(runs, vec![], vec![]);
        inherited.projection.insert(
            "visibleTurnItems".into(),
            json!([{"position": 0, "visibility": "inherited", "sourceThreadId": "parent",
                    "item": error_item("e6", "r2", "r2-root", "Parent failed", "unknown")}]),
        );
        assert_eq!(derive(&inherited), None);
    }

    #[test]
    fn missing_and_null_ids_match_only_their_like() {
        let mut no_root = run("r1", 1, "failed");
        no_root["rootNodeId"] = Value::Null;
        let mut null_node = error_item("e1", "r1", "", "Null node", "unknown");
        null_node["nodeId"] = Value::Null;
        let mut missing_node = error_item("e2", "r1", "", "Missing node", "unknown");
        missing_node.as_object_mut().unwrap().remove("nodeId");
        missing_node["updatedAt"] = json!("2026-10-09T11:00:00.000Z");
        let state = thread(vec![no_root], vec![null_node, missing_node], vec![]);
        // null matches null. A missing nodeId doesn't, though it is newer.
        assert_eq!(derive(&state), error("Null node", Some("unknown")));
    }

    #[test]
    fn the_latest_root_error_goes_by_time_then_ordinal_then_id() {
        let at = |mut item: Value, time: &str, ordinal: u64| {
            item["updatedAt"] = json!(time);
            item["ordinal"] = json!(ordinal);
            item
        };
        let runs = vec![run("r1", 1, "failed")];
        let early = at(
            error_item("z", "r1", "r1-root", "Early", "unknown"),
            "2026-10-09T10:00:00.000Z",
            9,
        );
        let late = at(
            error_item("a", "r1", "r1-root", "Late", "unknown"),
            "2026-10-09T10:00:00.500Z",
            1,
        );
        let state = thread(runs.clone(), vec![late.clone(), early.clone()], vec![]);
        assert_eq!(derive(&state).unwrap().message, "Late");

        let same = "2026-10-09T10:00:00.000Z";
        let low = at(error_item("b", "r1", "r1-root", "Low", "unknown"), same, 1);
        let high = at(error_item("a", "r1", "r1-root", "High", "unknown"), same, 2);
        let state = thread(runs.clone(), vec![high, low], vec![]);
        assert_eq!(derive(&state).unwrap().message, "High");

        let first = error_item("a", "r1", "r1-root", "First", "unknown");
        let second = error_item("b", "r1", "r1-root", "Second", "unknown");
        let (first, second) = (at(first, same, 1), at(second, same, 1));
        let state = thread(runs, vec![second, first], vec![]);
        assert_eq!(derive(&state).unwrap().message, "Second");
    }

    #[test]
    fn a_new_run_after_a_failure_clears_it() {
        let failure = error_item("e", "r1", "r1-root", "Boom", "provider_error");
        let state = thread(
            vec![run("r1", 1, "failed"), run("r2", 2, "running")],
            vec![failure.clone()],
            vec![],
        );
        assert_eq!(derive(&state), None);
        let state = thread(
            vec![run("r1", 1, "failed"), run("r2", 2, "completed")],
            vec![failure],
            vec![],
        );
        assert_eq!(derive(&state), None);
    }

    #[test]
    fn a_held_queue_doesnt_stand_for_the_thread() {
        let mut held = run("r2", 2, "queued");
        held["queueHeld"] = json!(true);
        let state = thread(
            vec![run("r1", 1, "failed"), held.clone()],
            vec![error_item("e", "r1", "r1-root", "Boom", "provider_error")],
            vec![],
        );
        assert_eq!(derive(&state), error("Boom", Some("provider_error")));
        // A queued run that isn't held is the newest, so it stands for the thread.
        held["queueHeld"] = json!(false);
        let state = thread(
            vec![run("r1", 1, "failed"), held],
            vec![error_item("e", "r1", "r1-root", "Boom", "provider_error")],
            vec![],
        );
        assert_eq!(derive(&state), None);
    }

    #[test]
    fn a_usage_limit_stays_the_outcome_while_newer_runs_wait() {
        let limit = error_item("e", "r1", "r1-root", "Usage limit reached", "usage_limit");
        let mut cancelled = run("r3", 3, "cancelled");
        cancelled["startedAt"] = Value::Null;
        let state = thread(
            vec![run("r1", 1, "failed"), run("r2", 2, "queued"), cancelled],
            vec![limit.clone()],
            vec![],
        );
        assert_eq!(
            derive(&state),
            error("Usage limit reached", Some("usage_limit"))
        );
        // Another class of failure doesn't hold back a newer queued run.
        let other = error_item("e", "r1", "r1-root", "Boom", "provider_error");
        let state = thread(
            vec![run("r1", 1, "failed"), run("r2", 2, "queued")],
            vec![other],
            vec![],
        );
        assert_eq!(derive(&state), None);
        // A run that started after the limit, and is still going, is the latest that ran.
        let mut going = run("r2", 2, "running");
        going.as_object_mut().unwrap().remove("completedAt");
        let state = thread(vec![run("r1", 1, "failed"), going], vec![limit], vec![]);
        assert_eq!(derive(&state), None);
    }

    #[test]
    fn the_latest_run_to_end_wins_over_a_higher_ordinal() {
        // r2 was resumed from a held queue and ended after r3, so it ran last.
        let mut resumed = run("r2", 2, "failed");
        resumed["completedAt"] = json!("2026-10-09T10:05:00.000Z");
        let finished = run("r3", 3, "completed");
        let mut queued = run("r4", 4, "queued");
        queued["queueHeld"] = json!(true);
        let state = thread(
            vec![resumed, finished, queued],
            vec![error_item("e", "r2", "r2-root", "Limit", "usage_limit")],
            vec![],
        );
        assert_eq!(derive(&state), error("Limit", Some("usage_limit")));
    }

    #[test]
    fn a_distinct_session_error_wins_and_drops_the_class() {
        let runs = vec![run("r1", 1, "failed")];
        let items = vec![error_item("e", "r1", "r1-root", "Limit", "usage_limit")];
        let other = thread(
            runs.clone(),
            items.clone(),
            vec![session("s", "codex", json!("Session died"))],
        );
        assert_eq!(derive(&other), error("Session died", None));
        // The same text keeps the failure's class.
        let same = thread(
            runs.clone(),
            items.clone(),
            vec![session("s", "codex", json!("Limit"))],
        );
        assert_eq!(derive(&same), error("Limit", Some("usage_limit")));
        // A session of another provider instance doesn't count, and the last match does.
        let sessions = vec![
            session("s1", "codex", json!("First")),
            session("s2", "claude", json!("Other provider")),
            session("s3", "codex", json!("Last")),
        ];
        let state = thread(runs.clone(), items.clone(), sessions);
        assert_eq!(derive(&state), error("Last", None));
        // A session error shows with no failed run at all.
        let state = thread(
            vec![run("r1", 1, "completed")],
            vec![],
            vec![session("s", "codex", json!("Auth expired"))],
        );
        assert_eq!(derive(&state), error("Auth expired", None));
    }

    #[test]
    fn null_empty_and_malformed_fields() {
        let runs = vec![run("r1", 1, "failed")];
        let items = vec![error_item("e", "r1", "r1-root", "Boom", "provider_error")];
        // A null or non-string lastError counts as none.
        for last_error in [Value::Null, json!(42), json!({"message": "x"})] {
            let state = thread(
                runs.clone(),
                items.clone(),
                vec![session("s", "codex", last_error)],
            );
            assert_eq!(derive(&state), error("Boom", Some("provider_error")));
        }
        // An empty session error stands in for the failure, as `??` keeps "". The banner
        // hides it.
        let state = thread(
            runs.clone(),
            items.clone(),
            vec![session("s", "codex", json!(""))],
        );
        assert_eq!(derive(&state), error("", None));
        // A failure without a message, or with no failure at all, has nothing to show.
        let mut no_message = items[0].clone();
        no_message["failure"] = json!({"class": "unknown"});
        let state = thread(runs.clone(), vec![no_message], vec![]);
        assert_eq!(derive(&state), None);
        let mut no_failure = items[0].clone();
        no_failure["failure"] = Value::Null;
        let state = thread(runs.clone(), vec![no_failure], vec![]);
        assert_eq!(derive(&state), None);
        // A failure without a class has a message and no class.
        let mut no_class = items[0].clone();
        no_class["failure"] = json!({"message": "Classless"});
        assert_eq!(
            derive(&thread(runs, vec![no_class], vec![])),
            error("Classless", None)
        );
        // Lists that aren't lists, and a thread with nothing in it, have no error.
        let state = ThreadState::from_snapshot(&json!({
            "kind": "snapshot",
            "projection": {"thread": {"id": "t"}, "runs": "nope", "turnItems": 7, "providerSessions": null},
        }))
        .unwrap();
        assert_eq!(derive(&state), None);
    }

    #[test]
    fn a_thread_that_never_ran_shows_a_session_error_only_with_a_provider_thread() {
        let sessions = vec![session("s", "codex", json!("Could not start"))];
        let mut state = thread(vec![], vec![], sessions);
        assert_eq!(derive(&state), error("Could not start", None));
        state.projection["thread"]["activeProviderThreadId"] = Value::Null;
        assert_eq!(derive(&state), None);
        // Only held runs is the same as none.
        let mut held = run("r1", 1, "queued");
        held["queueHeld"] = json!(true);
        state
            .projection
            .insert("runs".into(), Value::Array(vec![held]));
        assert_eq!(derive(&state), None);
    }

    // ---- when the error is worked out again ----

    fn event(sequence: u64, event_type: &str, payload: Value) -> Value {
        json!({"kind": "event", "sequence": sequence, "event": {"type": event_type, "payload": payload}})
    }

    fn snapshot(sequence: u64, state: &ThreadState) -> Value {
        let projection = Value::Object(state.projection.clone());
        json!({"kind": "snapshot", "snapshotSequence": sequence, "projection": projection})
    }

    /// Applies `item` as `OpenThread::apply` does, noting it for the error.
    fn apply(state: &mut ThreadState, derived: &mut Derived, item: Value) {
        let applied = state.apply(&item);
        derived.note(&applied, &item);
    }

    #[test]
    fn the_error_is_worked_out_again_only_after_an_event_that_can_change_it() {
        let mut state = thread(vec![run("r1", 1, "running")], vec![], vec![]);
        let mut derived = Derived::default();
        assert_eq!(derived.get(Some(&state)), None);
        assert_eq!(derived.derives, 1);
        // Frames read the kept error.
        for _ in 0..3 {
            derived.get(Some(&state));
        }
        assert_eq!(derived.derives, 1);

        // Pieces of a streamed answer don't change it.
        for sequence in 2..12 {
            let answer = json!({"id": "a", "type": "assistant_message", "runId": "r1",
                                "text": "x".repeat(sequence as usize)});
            apply(
                &mut state,
                &mut derived,
                event(sequence, "turn-item.updated", answer),
            );
            derived.get(Some(&state));
        }
        assert_eq!(derived.derives, 1);

        // The root error arrives, then its run fails.
        let failure = error_item("e", "r1", "r1-root", "Boom", "provider_error");
        apply(
            &mut state,
            &mut derived,
            event(12, "turn-item.updated", failure),
        );
        assert_eq!(derived.get(Some(&state)), None, "the run hasn't failed yet");
        assert_eq!(derived.derives, 2);
        apply(
            &mut state,
            &mut derived,
            event(13, "run.updated", run("r1", 1, "failed")),
        );
        assert_eq!(
            derived.get(Some(&state)).cloned(),
            error("Boom", Some("provider_error"))
        );
        assert_eq!(derived.derives, 3);

        // A replayed event changes nothing and isn't noted.
        apply(
            &mut state,
            &mut derived,
            event(13, "run.updated", run("r1", 1, "running")),
        );
        assert_eq!(derived.get(Some(&state)).unwrap().message, "Boom");
        assert_eq!(derived.derives, 3);

        // A provider session's error is noticed as it comes.
        let failing = session("s", "codex", json!("Session died"));
        apply(
            &mut state,
            &mut derived,
            event(14, "provider-session.attached", failing),
        );
        assert_eq!(derived.get(Some(&state)).unwrap().message, "Session died");
        let cleared = session("s", "codex", Value::Null);
        apply(
            &mut state,
            &mut derived,
            event(15, "provider-session.updated", cleared),
        );
        assert_eq!(derived.get(Some(&state)).unwrap().message, "Boom");

        // A detached session no longer stands for the thread.
        let failing = session("s", "codex", json!("Session died"));
        apply(
            &mut state,
            &mut derived,
            event(16, "provider-session.updated", failing),
        );
        assert_eq!(derived.get(Some(&state)).unwrap().message, "Session died");
        apply(
            &mut state,
            &mut derived,
            event(
                17,
                "provider-session.detached",
                json!({"providerSessionId": "s", "detachedAt": "2026-10-09T10:01:00.000Z"}),
            ),
        );
        assert_eq!(derived.get(Some(&state)).unwrap().message, "Boom");

        // The thread moving to another provider instance leaves the old one's session behind.
        apply(
            &mut state,
            &mut derived,
            event(
                18,
                "provider-session.attached",
                session("s2", "codex", json!("Codex down")),
            ),
        );
        assert_eq!(derived.get(Some(&state)).unwrap().message, "Codex down");
        apply(
            &mut state,
            &mut derived,
            event(
                19,
                "thread.updated",
                json!({"id": "t", "providerInstanceId": "claude", "activeProviderThreadId": "pt"}),
            ),
        );
        assert_eq!(derived.get(Some(&state)).unwrap().message, "Boom");

        // A fresh snapshot on reconnect replaces everything, so it is read again.
        let derives = derived.derives;
        let fresh = thread(vec![run("r1", 1, "completed")], vec![], vec![]);
        let item = snapshot(30, &fresh);
        apply(&mut state, &mut derived, item);
        assert_eq!(derived.get(Some(&state)), None);
        assert_eq!(derived.derives, derives + 1);
    }

    // ---- what the banner shows ----

    fn shown(banner: &Banner) -> Option<(&str, bool)> {
        banner
            .shown
            .as_ref()
            .map(|shown| (shown.text.as_str(), shown.warning))
    }

    fn runtime(message: &str, class: Option<&str>) -> RuntimeError {
        error(message, class).unwrap()
    }

    #[test]
    fn no_error_shows_no_banner() {
        let mut banner = Banner::default();
        banner.follow(Some("a"), None);
        assert_eq!(shown(&banner), None);
        banner.follow(None, Some(&runtime("Boom", None)));
        assert_eq!(shown(&banner), None, "no thread is open");
    }

    #[test]
    fn a_dismissed_error_stays_hidden_on_its_thread_until_its_text_changes() {
        let mut banner = Banner::default();
        let boom = runtime("Boom", Some("provider_error"));
        banner.follow(Some("a"), Some(&boom));
        assert_eq!(shown(&banner), Some(("Boom", false)));
        banner.dismiss();
        banner.follow(Some("a"), Some(&boom));
        assert_eq!(shown(&banner), None);

        // Away to a thread with no error and back: still dismissed.
        banner.follow(Some("b"), None);
        assert_eq!(shown(&banner), None);
        banner.follow(Some("a"), Some(&boom));
        assert_eq!(shown(&banner), None);

        // Another thread with the same text shows it.
        banner.follow(Some("b"), Some(&boom));
        assert_eq!(shown(&banner), Some(("Boom", false)));

        // A different error on the first thread shows, and the old text stays dismissed.
        banner.follow(Some("a"), Some(&runtime("Boom again", None)));
        assert_eq!(shown(&banner), Some(("Boom again", false)));
        banner.follow(Some("a"), Some(&boom));
        assert_eq!(shown(&banner), None);

        // Dismissal compares the raw text, so a change only in spacing is a new error.
        banner.follow(Some("a"), Some(&runtime("Boom ", None)));
        assert_eq!(shown(&banner), Some(("Boom", false)));
    }

    #[test]
    fn a_send_error_stands_before_the_threads_own_and_dismissing_it_reveals_that() {
        let mut banner = Banner::default();
        let boom = runtime("Limit", Some("usage_limit"));
        banner.failed("a", "Timed out waiting for T3".into(), "m1".into());
        banner.follow(Some("a"), Some(&boom));
        assert_eq!(shown(&banner), Some(("Timed out waiting for T3", false)));
        banner.dismiss();
        banner.follow(Some("a"), Some(&boom));
        // The usage limit was under it, and it shows as a warning.
        assert_eq!(shown(&banner), Some(("Limit", true)));

        // The same send error again stays dismissed, and hides T3's error as on the desktop.
        banner.failed("a", "Timed out waiting for T3".into(), "m2".into());
        banner.follow(Some("a"), Some(&boom));
        assert_eq!(shown(&banner), None);

        // A new send to the thread clears its send error.
        banner.clear_local("a");
        banner.follow(Some("a"), Some(&boom));
        assert_eq!(shown(&banner), Some(("Limit", true)));
    }

    #[test]
    fn a_send_error_waits_for_its_own_thread() {
        let mut banner = Banner::default();
        // The send to a failed while b was open.
        banner.follow(Some("b"), None);
        banner.failed("a", "Couldn't reach T3".into(), "m1".into());
        banner.follow(Some("b"), None);
        assert_eq!(shown(&banner), None);
        banner.follow(Some("a"), None);
        assert_eq!(shown(&banner), Some(("Couldn't reach T3", false)));
        // It goes once the thread shows the message after all.
        banner.landed("a", |id| id == "m0");
        banner.follow(Some("a"), None);
        assert!(shown(&banner).is_some());
        banner.landed("a", |id| id == "m1");
        banner.follow(Some("a"), None);
        assert_eq!(shown(&banner), None);
    }

    #[test]
    fn empty_and_control_only_errors_show_nothing() {
        let mut banner = Banner::default();
        for raw in [
            "",
            " ",
            "\u{1b}",
            "\u{1b}[2J\u{7}",
            "\r\n\t",
            "\u{9b}",
            "\u{200b}\u{202e}\n\u{200d}",
        ] {
            banner.follow(Some("a"), Some(&runtime(raw, None)));
            let text = shown(&banner).map(|(text, _)| text.to_string());
            if raw == "\u{1b}[2J\u{7}" {
                // What the sequence leaves behind is plain text.
                assert_eq!(text.as_deref(), Some("[2J"));
            } else {
                assert_eq!(text, None, "{raw:?}");
            }
        }
        // An empty send error hides T3's error too.
        banner.failed("a", String::new(), "m1".into());
        banner.follow(Some("a"), Some(&runtime("Boom", None)));
        assert_eq!(shown(&banner), None);
    }

    #[test]
    fn a_send_error_never_shows_as_a_warning() {
        let mut banner = Banner::default();
        banner.failed("a", "Limit".into(), "m1".into());
        banner.follow(Some("a"), Some(&runtime("Limit", Some("usage_limit"))));
        assert_eq!(shown(&banner), Some(("Limit", false)));
    }

    #[test]
    fn the_same_error_keeps_the_banner_and_its_text() {
        let mut banner = Banner::default();
        let boom = runtime("Boom", None);
        banner.follow(Some("a"), Some(&boom));
        banner.toggle();
        for _ in 0..5 {
            banner.follow(Some("a"), Some(&boom));
        }
        assert_eq!(banner.prepares, 1);
        assert!(banner.shown.as_ref().unwrap().expanded);
        // The class can change without the text.
        banner.follow(Some("a"), Some(&runtime("Boom", Some("usage_limit"))));
        assert_eq!(shown(&banner), Some(("Boom", true)));
        assert_eq!(banner.prepares, 1);
        // Another thread, or a new text, starts closed.
        banner.follow(Some("b"), Some(&boom));
        assert!(!banner.shown.as_ref().unwrap().expanded);
    }

    #[test]
    fn a_long_error_is_cut_at_a_character_and_cleaned() {
        // Three-byte characters, so byte 4096, one past 4095, falls inside one.
        let raw = "界".repeat(3000);
        let text = prepare(&raw);
        let (body, note) = text.rsplit_once('\n').unwrap();
        assert_eq!(body.len(), 4095);
        assert!(body.chars().all(|c| c == '界'));
        assert_eq!(note, "… the error goes on past 4096 bytes");
        // Controls go, line breaks stay, and a carriage return can't overwrite the line.
        assert_eq!(
            prepare("\u{1b}]52;c;aGk=\u{7}first\r\nsecond\rthird\ttab\u{0}"),
            "]52;c;aGk=first\nsecond\nthird tab"
        );
        // An error under the limit is kept whole.
        assert_eq!(prepare("short"), "short");
        // Nothing a terminal acts on is left, whatever the error held.
        let every: String = (0..=0x9f_u32).filter_map(char::from_u32).collect();
        let cleaned = prepare(&format!("start{every}end"));
        assert!(!cleaned.chars().any(|c| c.is_control() && c != '\n'));
        assert!(cleaned.starts_with("start") && cleaned.ends_with("end"));
    }

    // ---- drawing ----

    /// Draws `banner` over a whole screen `width` by `height` and returns what is on it.
    fn draw_cells(banner: &mut Banner, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let theme = Theme::new(Depth::TrueColor);
        let frame = terminal
            .draw(|frame| {
                let area = frame.area();
                banner.draw(frame, area, &theme);
            })
            .unwrap();
        frame.buffer.clone()
    }

    /// Each row of the screen as text. A wide character's second cell reads as a space.
    fn draw(banner: &mut Banner, width: u16, height: u16) -> Vec<String> {
        let buffer = draw_cells(banner, width, height);
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    fn showing(raw: &str) -> Banner {
        let mut banner = Banner::default();
        banner.follow(Some("a"), Some(&runtime(raw, None)));
        banner
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    const CLICK: MouseEventKind = MouseEventKind::Down(MouseButton::Left);

    #[test]
    fn a_short_error_fits_its_text_and_names_its_keys() {
        let mut banner = showing("Boom");
        let rows = draw(&mut banner, 80, 20);
        let area = banner.area;
        // Centered, three rows tall: the border, the text and the border with its hint.
        assert_eq!(area.height, 3);
        assert_eq!(area.y, 0);
        assert!(area.x > 0 && area.x + area.width < 80);
        let text_row = &rows[1];
        assert!(text_row.contains(" ! Boom"), "{rows:#?}");
        assert!(text_row.contains(" × "));
        assert!(rows[2].contains("Alt+W dismiss"), "{rows:#?}");
        assert!(!rows[2].contains("Alt+I"), "nothing is cut");
        // The rows below it are left alone.
        assert!(rows[3..].iter().all(|row| row.trim().is_empty()));
        // × is on the text row, inside the banner.
        let close = banner.close;
        assert_eq!(close.y, 1);
        assert!(close.x + close.width <= area.x + area.width);
        let cells: Vec<char> = text_row.chars().collect();
        let start = close.x as usize;
        assert!(cells[start..start + close.width as usize].contains(&'×'));
    }

    #[test]
    fn a_long_error_shows_three_rows_and_opens_with_alt_i() {
        let raw = (1..=12)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut banner = showing(&raw);
        let rows = draw(&mut banner, 60, 30);
        assert_eq!(banner.area.height, 5, "three rows of text and the borders");
        assert!(rows[1].contains("line 1"), "{rows:#?}");
        assert!(rows[3].contains("line 3…"), "{rows:#?}");
        assert!(rows[4].contains("Alt+I more · Alt+W dismiss"), "{rows:#?}");
        assert!(banner.clipped);

        let alt = KeyModifiers::ALT;
        assert!(banner.on_key(&key(KeyCode::Char('i'), alt)));
        let rows = draw(&mut banner, 60, 30);
        assert_eq!(banner.area.height, 14, "all twelve lines");
        assert!(rows[12].contains("line 12"), "{rows:#?}");
        assert!(rows[13].contains("Alt+I less"), "{rows:#?}");
        assert!(!banner.overflows);

        // On a screen too short for all of it, the open error scrolls.
        let rows = draw(&mut banner, 60, 8);
        assert_eq!(banner.area.height, 8);
        assert!(banner.overflows);
        assert!(rows[7].contains("Lines 1-6 of 12"), "{rows:#?}");
        assert!(banner.on_key(&key(KeyCode::Down, alt)));
        let rows = draw(&mut banner, 60, 8);
        assert!(rows[1].contains("line 2"), "{rows:#?}");
        assert!(rows[7].contains("Lines 2-7 of 12"), "{rows:#?}");
        // The scroll stops at the end.
        for _ in 0..20 {
            banner.on_key(&key(KeyCode::Down, alt));
        }
        let rows = draw(&mut banner, 60, 8);
        assert!(rows[6].contains("line 12"), "{rows:#?}");
        assert!(rows[7].contains("Lines 7-12 of 12"), "{rows:#?}");
        // The wheel scrolls it too.
        let x = banner.area.x + 2;
        assert_eq!(
            banner.on_mouse(&mouse(MouseEventKind::ScrollUp, x, 3)),
            Some(true)
        );
        let rows = draw(&mut banner, 60, 8);
        assert!(rows[7].contains("Lines 6-11 of 12"), "{rows:#?}");

        // Alt+I closes it again, back at the top.
        let shift_i = key(KeyCode::Char('I'), alt | KeyModifiers::SHIFT);
        assert!(banner.on_key(&shift_i));
        let rows = draw(&mut banner, 60, 30);
        assert!(rows[1].contains("line 1"));
        let x = banner.area.x + 2;
        // A closed banner leaves the wheel to the transcript under it.
        assert_eq!(
            banner.on_mouse(&mouse(MouseEventKind::ScrollDown, x, 2)),
            None
        );
        assert!(!banner.on_key(&key(KeyCode::Down, alt)));
    }

    #[test]
    fn the_banner_takes_its_clicks() {
        let raw = "word ".repeat(100);
        let mut banner = showing(&raw);
        draw(&mut banner, 60, 20);
        let area = banner.area;
        // A click outside isn't the banner's.
        let below = mouse(CLICK, 0, area.y + area.height);
        assert_eq!(banner.on_mouse(&below), None);
        // A click on the cut text opens it, and a second closes it.
        assert_eq!(banner.on_mouse(&mouse(CLICK, area.x + 3, 2)), Some(true));
        assert!(banner.shown.as_ref().unwrap().expanded);
        draw(&mut banner, 60, 20);
        assert_eq!(banner.on_mouse(&mouse(CLICK, area.x + 3, 2)), Some(true));
        assert!(!banner.shown.as_ref().unwrap().expanded);
        // Other buttons and a release are taken and do nothing.
        draw(&mut banner, 60, 20);
        let right = MouseEventKind::Down(MouseButton::Right);
        assert_eq!(banner.on_mouse(&mouse(right, area.x + 3, 2)), Some(false));
        // × dismisses it.
        let close = banner.close;
        assert_eq!(banner.on_mouse(&mouse(CLICK, close.x, close.y)), Some(true));
        assert_eq!(shown(&banner), None);
        assert_eq!(banner.area, Rect::default());
        assert_eq!(banner.on_mouse(&mouse(CLICK, close.x, close.y)), None);

        // A short error's text isn't cut, so a click on it does nothing but is still taken.
        let mut banner = showing("Boom");
        draw(&mut banner, 60, 20);
        let area = banner.area;
        assert_eq!(banner.on_mouse(&mouse(CLICK, area.x + 3, 1)), Some(false));
        assert!(!banner.shown.as_ref().unwrap().expanded);
    }

    #[test]
    fn the_keys_are_the_banners_only_while_it_shows() {
        let alt = KeyModifiers::ALT;
        let mut banner = showing("Boom");
        // Not drawn yet, so not on screen.
        assert!(!banner.on_key(&key(KeyCode::Char('w'), alt)));
        draw(&mut banner, 60, 20);
        // Plain letters, Ctrl+Alt and other Alt keys go on.
        assert!(!banner.on_key(&key(KeyCode::Char('w'), KeyModifiers::NONE)));
        let ctrl_alt_w = key(KeyCode::Char('w'), alt | KeyModifiers::CONTROL);
        assert!(!banner.on_key(&ctrl_alt_w));
        assert!(!banner.on_key(&key(KeyCode::Char('t'), alt)));
        assert!(!banner.on_key(&key(KeyCode::Up, alt)), "nothing to scroll");
        assert!(banner.on_key(&key(KeyCode::Char('w'), alt)));
        assert_eq!(shown(&banner), None);
        assert!(!banner.on_key(&key(KeyCode::Char('w'), alt)));
        assert!(!banner.on_key(&key(KeyCode::Char('i'), alt)));
    }

    #[test]
    fn a_frame_rewraps_only_for_a_new_width() {
        let mut banner = showing(&"word ".repeat(400));
        for _ in 0..3 {
            draw(&mut banner, 80, 24);
        }
        assert_eq!(banner.wraps, 1);
        banner.toggle();
        draw(&mut banner, 80, 24);
        draw(&mut banner, 80, 10);
        assert_eq!(banner.wraps, 1, "height and opening don't change the width");
        draw(&mut banner, 70, 24);
        assert_eq!(banner.wraps, 2);
    }

    #[test]
    fn wide_joined_and_control_text_stays_inside_the_banner_at_any_size() {
        let nasty = format!(
            "{}\n\u{1b}[31mred\u{1b}[0m 👩‍💻👍🏽 e\u{301}\u{301} ⚠\u{fe0f}\t\u{202e}rtl\n{}\n{}",
            "界".repeat(500),
            "a".repeat(300),
            "\n".repeat(50)
        );
        let sizes = [
            (0, 0),
            (1, 1),
            (2, 2),
            (4, 3),
            (5, 1),
            (6, 2),
            (11, 3),
            (12, 3),
            (13, 6),
            (14, 5),
            (40, 4),
            (80, 24),
            (200, 60),
        ];
        for expanded in [false, true] {
            for (width, height) in sizes {
                let size = format!("{width}x{height}, open: {expanded}");
                let mut banner = showing(&nasty);
                if expanded {
                    banner.toggle();
                }
                let cells = draw_cells(&mut banner, width, height);
                let screen = Rect::new(0, 0, width, height);
                let (area, close) = (banner.area, banner.close);
                assert_eq!(area.intersection(screen), area, "{size}");
                assert_eq!(close.intersection(area), close, "{size}");
                // Nothing is drawn outside the banner.
                for y in 0..height {
                    for x in 0..width {
                        if !area.contains(Position::new(x, y)) {
                            assert_eq!(cells[(x, y)].symbol(), " ", "{size} at {x},{y}");
                        }
                    }
                }
                if width < 5 || height == 0 {
                    assert!(!banner.showing(), "{size}");
                    continue;
                }
                assert!(banner.showing(), "{size}");
                // No row of text runs over ×, however wide its characters.
                let y = close.y;
                let on_close = (close.x..close.right()).any(|x| cells[(x, y)].symbol() == "×");
                assert!(on_close, "{size}");
                if !expanded {
                    // A closed banner leaves at least half the transcript.
                    assert!(area.height <= (height / 2).max(1), "{size}");
                }
            }
        }
    }

    #[test]
    fn a_usage_limit_draws_as_a_warning() {
        let theme = Theme::new(Depth::TrueColor);
        let mut banner = Banner::default();
        banner.follow(Some("a"), Some(&runtime("Limit", Some("usage_limit"))));
        let cells = draw_cells(&mut banner, 40, 10);
        assert_eq!(cells[(banner.area.x, 0)].fg, theme.warning);

        let mut banner = showing("Boom");
        let cells = draw_cells(&mut banner, 40, 10);
        assert_eq!(cells[(banner.area.x, 0)].fg, theme.error);
    }
}
