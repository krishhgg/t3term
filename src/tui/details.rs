//! The open thread's details: its project, where it works, the branch T3 recorded for it and
//! the environment. They show in a pane beside the conversation when the screen has room for
//! one, and in a dialog over it when it hasn't. Alt+B and the button at the right of the title
//! row open and close them. They start closed, and only this window keeps whether they are
//! open.
//!
//! The desktop shows a thread's workspace in a card at the right of the chat
//! (`ThreadDetailsPanel` in the nightly's apps/web/src/components/chat/ThreadDetailsPanel.tsx).
//! The card sits inline while the chat keeps its `minChatWidth` beside it, and opens as a
//! popover otherwise (`resolveThreadDetailsCardLayout` in threadDetailsCardLayout.ts). It names
//! the workspace by the last folder of the thread's worktree path, or of the project's folder
//! for a thread without one, and tags it Worktree only when the thread has a worktree path
//! (`resolveWorkspaceDisplayName` and `resolveLockedWorkspaceLabel` in
//! apps/web/src/components/BranchToolbar.logic.ts). Its branch row reads Git first. t3term
//! reads no Git and no files, so it shows the branch T3 recorded for the thread and labels it
//! as recorded.
//!
//! The values come from what t3term already holds: the open thread's projection once its watch
//! sends it, the shell's copy of the thread until then, and the shell's projects. They are
//! worked out again only after an event that can change them, and only while the details are
//! open, so closed details cost nothing. A frame wraps them again only at a new width.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::environment::Identity;
use super::setup::bounded;
use super::tasks::wrap;
use super::theme::Theme;
use super::{Focus, clip, row};
use crate::projection::Applied;
use crate::transcript::{clusters, without_controls};

/// The button at the right end of the title row, a mirror of the sidebar's.
pub(super) const BUTTON: &str = " ◨ ";
/// The pane's columns, its borders included: the desktop's 280-pixel card at about eight
/// pixels a column.
pub(super) const PANE_WIDTH: u16 = 36;
/// The blank columns between the conversation and the pane.
const GAP: u16 = 1;
/// The fewest columns the conversation keeps beside the pane. With fewer, the details open as
/// a dialog, as the desktop's card turns into a popover when the chat would get narrower than
/// its `minChatWidth`.
const NARROWEST_CONVERSATION: u16 = 60;
/// The rows of the main column that aren't the conversation's: the title row, the blank row
/// under it and the status line.
const CHROME_ROWS: u16 = 3;
/// The fewest rows the pane gets. With fewer, the details open as a dialog, as the desktop's
/// card turns into a popover below 160 pixels.
const SHORTEST_PANE: u16 = 12;
/// The dialog's widest, its borders included.
const DIALOG_WIDTH: u16 = 56;
/// The narrowest card with a border. A narrower one draws its rows straight on the screen.
const BORDER_MIN_WIDTH: u16 = 12;
/// The most characters kept of a project's title, a branch or a folder's name.
const NAME: usize = 256;
/// The most characters kept of a path: its first `PATH_HEAD` and its last `PATH_TAIL`, with
/// `…` between, so the folder at its end stays.
const PATH_HEAD: usize = 256;
const PATH_TAIL: usize = 768;
/// The most rows a name or a branch wraps to, and a path.
const VALUE_ROWS: usize = 3;
const PATH_ROWS: usize = 8;
/// The longest project id kept to match project events against. With a longer one, every
/// project event counts.
const ID: usize = 512;

/// How the details draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Layout {
    /// Beside the conversation, which narrows to make room.
    Pane,
    /// Over the conversation, under the title row at the right.
    Dialog,
}

/// How the details fit the main column `main`, its padding already taken off: as a pane when
/// the conversation keeps `NARROWEST_CONVERSATION` columns beside it and the pane gets
/// `SHORTEST_PANE` rows, and as a dialog otherwise.
pub(super) fn layout(main: Rect) -> Layout {
    let rows = main.height.saturating_sub(CHROME_ROWS);
    if main.width >= PANE_WIDTH + GAP + NARROWEST_CONVERSATION && rows >= SHORTEST_PANE {
        Layout::Pane
    } else {
        Layout::Dialog
    }
}

/// The conversation's part of `main` while the pane shows.
pub(super) fn beside(main: Rect) -> Rect {
    Rect {
        width: main.width.saturating_sub(PANE_WIDTH + GAP),
        ..main
    }
}

/// The pane, at the right of `main` from row `top` down to row `bottom`, not included.
pub(super) fn pane_area(main: Rect, top: u16, bottom: u16) -> Rect {
    let width = PANE_WIDTH.min(main.width);
    Rect {
        x: main.right() - width,
        y: top,
        width,
        height: bottom.saturating_sub(top),
    }
}

/// The open thread's record as t3term holds it.
pub(super) enum Record<'a> {
    /// No thread is open.
    None,
    /// The thread's record, from its projection or the shell.
    Found(&'a Value),
    /// The thread's watch hasn't sent it yet, and the shell doesn't list it.
    Loading,
    /// Neither has it, and the watch has nothing more to send: a projection without the
    /// thread, or a watch that closed before one came.
    Unavailable,
}

/// The shell's projects, or why there are none yet.
pub(super) enum Projects<'a> {
    /// The shell hasn't sent its snapshot yet.
    Loading,
    /// T3 refused the shell's subscription, so no projects will come.
    Failed,
    Listed(&'a [Value]),
}

/// One value under a label, cleaned and bounded, and how many rows it may wrap to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    text: String,
    /// A note of t3term's own, such as `Unavailable`, rather than text from T3.
    note: bool,
    rows: usize,
}

impl Piece {
    fn value(text: String, rows: usize) -> Piece {
        Piece {
            text,
            note: false,
            rows,
        }
    }

    fn note(text: &str) -> Piece {
        Piece {
            text: text.to_string(),
            note: true,
            rows: VALUE_ROWS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Section {
    label: &'static str,
    pieces: Vec<Piece>,
}

/// What the details show for one thread, worked out from its record and the shell's projects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Facts {
    /// The open thread these describe. None when no thread is open.
    thread_id: Option<String>,
    /// The thread's project, to match project events against. None when the record names none
    /// or one longer than `ID`.
    project_id: Option<String>,
    sections: Vec<Section>,
}

impl Facts {
    /// The details of thread `open_id`, the open one, from its `record` and the shell's
    /// `projects`. Each value is cleaned and bounded here, once, so a frame only wraps them.
    ///
    /// The workspace follows the desktop's rules. A worktree path makes it a worktree, at that
    /// path. A thread whose worktree path is null works in the local checkout, the project's
    /// folder. A worktree path that is missing, blank or not a string leaves the workspace
    /// unavailable, and a blank project folder leaves the checkout without one. `blank` reads
    /// only the ends of a long path. The branch is the one T3 recorded. A null branch reads
    /// `None recorded` and is never taken for main, and one missing or not a string reads
    /// `Unavailable`.
    pub(super) fn of(open_id: Option<&str>, record: Record, projects: Projects) -> Facts {
        let Some(open_id) = open_id else {
            return Facts::default();
        };
        let thread = match record {
            Record::None => return Facts::default(),
            Record::Found(thread) => thread,
            Record::Loading | Record::Unavailable => {
                let word = if matches!(record, Record::Loading) {
                    "Loading…"
                } else {
                    "Unavailable"
                };
                let note = |label| Section {
                    label,
                    pieces: vec![Piece::note(word)],
                };
                return Facts {
                    thread_id: Some(open_id.to_string()),
                    project_id: None,
                    sections: vec![note("Project"), note("Workspace"), note("Recorded branch")],
                };
            }
        };
        let project_id = thread.get("projectId").and_then(Value::as_str);
        let listed = match projects {
            Projects::Listed(listed) => Some(listed),
            Projects::Loading | Projects::Failed => None,
        };
        let project = project_id.zip(listed).and_then(|(id, listed)| {
            listed
                .iter()
                .find(|project| project.get("id").and_then(Value::as_str) == Some(id))
        });
        // A project the shell hasn't listed yet is on its way, until the shell's snapshot
        // comes without it or never comes.
        let pending = project_id.is_some() && matches!(projects, Projects::Loading);
        let unknown = |what: &str| {
            if pending {
                Piece::note("Loading…")
            } else {
                Piece::note(what)
            }
        };

        let title = match project {
            Some(project) => match project.get("title").and_then(Value::as_str) {
                Some(title) => {
                    let title = bounded(title, NAME);
                    if title.is_empty() {
                        Piece::note("Untitled")
                    } else {
                        Piece::value(title, VALUE_ROWS)
                    }
                }
                None => Piece::note("Untitled"),
            },
            None => unknown("Unavailable"),
        };

        let workspace = match thread.get("worktreePath") {
            Some(Value::String(path)) if !blank(path) => folder("Worktree", path),
            Some(Value::Null) => {
                let root = project
                    .and_then(|project| project.get("workspaceRoot"))
                    .and_then(Value::as_str)
                    .filter(|root| !blank(root));
                match root {
                    Some(root) => folder("Local checkout", root),
                    None => vec![
                        Piece::value("Local checkout".into(), VALUE_ROWS),
                        unknown("Project folder unavailable"),
                    ],
                }
            }
            _ => vec![Piece::note("Unavailable")],
        };

        let branch = match thread.get("branch") {
            Some(Value::String(branch)) => {
                let branch = bounded(branch, NAME);
                if branch.is_empty() {
                    Piece::note("Unavailable")
                } else {
                    Piece::value(branch, VALUE_ROWS)
                }
            }
            Some(Value::Null) => Piece::note("None recorded"),
            _ => Piece::note("Unavailable"),
        };

        Facts {
            thread_id: Some(open_id.to_string()),
            project_id: project_id.filter(|id| id.len() <= ID).map(str::to_string),
            sections: vec![
                Section {
                    label: "Project",
                    pieces: vec![title],
                },
                Section {
                    label: "Workspace",
                    pieces: workspace,
                },
                Section {
                    label: "Recorded branch",
                    pieces: vec![branch],
                },
            ],
        }
    }
}

/// A workspace of `kind` at `path`: its kind and folder's name on one row, the path under it.
/// A path that cleans to nothing leaves the kind alone.
fn folder(kind: &str, path: &str) -> Vec<Piece> {
    let name = folder_name(path);
    let head = if name.is_empty() {
        kind.to_string()
    } else {
        format!("{kind} · {name}")
    };
    let mut pieces = vec![Piece::value(head, VALUE_ROWS)];
    let text = path_text(path);
    if !text.is_empty() {
        pieces.push(Piece {
            text,
            note: true,
            rows: PATH_ROWS,
        });
    }
    pieces
}

/// The last folder of `path`, as the desktop's `resolveWorkspaceDisplayName` names a
/// workspace: slashes and backslashes at the end dropped, then what follows the last of
/// either. A path of nothing but slashes names itself. It reads at most the path's last
/// `PATH_TAIL` characters, and keeps at most `NAME` of the name.
fn folder_name(path: &str) -> String {
    let (end, _) = tail(path, PATH_TAIL);
    let trimmed = end.trim_end_matches(separator);
    if trimmed.is_empty() {
        return bounded(end, NAME);
    }
    let name = trimmed.rsplit(separator).next().unwrap_or(trimmed);
    bounded(name, NAME)
}

/// Whether `c` separates folders in a path, on macOS, Linux or Windows.
fn separator(c: char) -> bool {
    c == '/' || c == '\\'
}

/// `path` as one row of plain text, cleaned as `bounded` cleans it. A path longer than
/// `PATH_HEAD` and `PATH_TAIL` together keeps its first and last characters with `…` between,
/// so the folder at its end stays.
fn path_text(path: &str) -> String {
    let kept = match kept(path) {
        (whole, None) => whole.to_string(),
        (start, Some(end)) => format!("{start}…{end}"),
    };
    without_controls(&kept)
        .replace('\n', " ")
        .trim()
        .to_string()
}

/// Whether `path` has nothing but whitespace in the parts of it that `kept` keeps. A path
/// with no more characters than `PATH_HEAD` and `PATH_TAIL` together is read whole, so it is
/// blank when it trims to nothing. A longer one is blank when both its kept ends are, whatever
/// its middle holds, since the details would show none of that middle. Like a short blank
/// path, it leaves a worktree unavailable and a local checkout without its folder.
fn blank(path: &str) -> bool {
    let (start, end) = kept(path);
    start.trim().is_empty() && end.is_none_or(|end| end.trim().is_empty())
}

/// The parts of `path` the details read: all of it when it has no more characters than
/// `PATH_HEAD` and `PATH_TAIL` together, and otherwise its first `PATH_HEAD` and its last
/// `PATH_TAIL`. The second part is None for a path read whole.
fn kept(path: &str) -> (&str, Option<&str>) {
    let (whole, longer) = head(path, PATH_HEAD + PATH_TAIL);
    if longer {
        (head(path, PATH_HEAD).0, Some(tail(path, PATH_TAIL).0))
    } else {
        (whole, None)
    }
}

/// The start of `text`, at most `max` characters of it in whole pieces as `clusters` finds
/// them, and whether `text` goes on. It reads at most four characters for each it keeps.
fn head(text: &str, max: usize) -> (&str, bool) {
    let window = match text.char_indices().nth(max.saturating_mul(4)) {
        Some((at, _)) => &text[..at],
        None => text,
    };
    let mut kept = 0;
    let mut end = 0;
    for piece in clusters(window) {
        let count = piece.chars().count();
        if kept + count > max {
            break;
        }
        kept += count;
        end += piece.len();
    }
    (&text[..end], end < text.len())
}

/// The end of `text`, at most `max` characters of it in whole pieces as `clusters` finds them,
/// and whether `text` has more before it. It reads at most four characters for each it keeps.
fn tail(text: &str, max: usize) -> (&str, bool) {
    let start = match text.char_indices().rev().nth(max.saturating_mul(4)) {
        Some((at, _)) => at,
        None => 0,
    };
    let pieces = clusters(&text[start..]);
    // A window that starts inside the text may start inside a piece, whose start it lacks.
    let skip = usize::from(start > 0);
    let mut kept = 0;
    let mut begin = text.len();
    for piece in pieces.iter().skip(skip).rev() {
        let count = piece.chars().count();
        if kept + count > max {
            break;
        }
        kept += count;
        begin -= piece.len();
    }
    (&text[begin..], begin > 0)
}

/// The facts wrapped at one width.
struct Rows {
    width: usize,
    lines: Vec<Line<'static>>,
}

/// Where the last frame drew the details, for the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Drawn {
    pub layout: Layout,
    /// The whole card, its border included.
    pub area: Rect,
    /// The × at the right of its title row.
    pub close: Rect,
}

/// The details as the reader left them in this window.
#[derive(Default)]
pub(super) struct Details {
    /// Whether the reader opened them.
    pub open: bool,
    facts: Option<Facts>,
    /// Set when an event may have changed the facts, so the next frame that draws them works
    /// them out again.
    stale: bool,
    rows: Option<Rows>,
    /// Rows the card's text is scrolled down. The next frame stops it at the end.
    scroll: usize,
    /// The rows of text the last frame showed, which a page scrolls by.
    page: usize,
    /// How the last frame drew them. It stays after a resize until a frame draws them again,
    /// so keys still reach a dialog that shows.
    shown: Option<Layout>,
    /// Where the last frame drew them.
    pub drawn: Option<Drawn>,
    /// Set when what the last frame drew may have moved: the facts changed, another thread
    /// opened, the details opened or closed, or the screen resized. Until the next frame, the
    /// mouse acts on none of it.
    pub moved: bool,
    /// The button at the right of the title row.
    pub button: Rect,
    /// The pane that had the keyboard before the details took it.
    pub return_focus: Option<Focus>,
    /// Whether a dialog took the keyboard, rather than the reader with a click or Tab.
    pub taken: bool,
    /// How many times the facts were worked out and wrapped, so the tests can tell what a
    /// frame or an event did.
    #[cfg(test)]
    pub prepares: usize,
    #[cfg(test)]
    pub wraps: usize,
}

impl Details {
    /// Opens or closes the details. They open at the top of their text.
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.scroll = 0;
        self.shown = None;
        self.moved = true;
    }

    /// Notes that the facts may have changed, as a thread's watch reconnecting or closing can.
    pub fn mark(&mut self) {
        self.stale = true;
        self.moved = true;
    }

    /// Notes what an item from the shell's feed changed: a snapshot, or an event for the open
    /// thread, `open_id`, or for its project. Others, such as an enrichment of a project's
    /// repository or an event for another thread, leave the facts as they are.
    pub fn note_shell(&mut self, applied: &Applied, item: &Value, open_id: Option<&str>) {
        let names_open = |id: Option<&Value>| {
            open_id.is_some_and(|open_id| id.and_then(Value::as_str) == Some(open_id))
        };
        let changed = match applied {
            Applied::Snapshot => true,
            Applied::Event(kind) => match kind.as_str() {
                "thread.updated" => names_open(item.pointer("/thread/id")),
                "thread.removed" => names_open(item.get("threadId")),
                "project.updated" => self.names_project(item.pointer("/project/id")),
                "project.removed" => self.names_project(item.get("projectId")),
                _ => false,
            },
            _ => false,
        };
        if changed {
            self.mark();
        }
    }

    /// Notes what an item from the open thread's watch changed: a snapshot, an event for the
    /// thread itself, or the watch saying it caught up, which ends the wait for a thread it
    /// never sent. Each piece of a streamed answer and each run event leave the facts as they
    /// are.
    pub fn note_thread(&mut self, applied: &Applied) {
        let changed = match applied {
            Applied::Snapshot | Applied::Synchronized => true,
            Applied::Event(kind) => kind.starts_with("thread."),
            _ => false,
        };
        if changed {
            self.mark();
        }
    }

    /// Whether a project event naming `id` can change the facts: it names their project, or
    /// they don't know theirs yet.
    fn names_project(&self, id: Option<&Value>) -> bool {
        match self
            .facts
            .as_ref()
            .and_then(|facts| facts.project_id.as_deref())
        {
            Some(project_id) => id.and_then(Value::as_str) == Some(project_id),
            None => true,
        }
    }

    /// Follows another thread that opened. Its facts are worked out before a frame draws the
    /// details again, so the last thread's never show for it.
    pub fn follow(&mut self) {
        self.mark();
        self.scroll = 0;
    }

    /// Whether this frame has to work the facts out for thread `open_id`: the details are
    /// open, and an event may have changed them since, or they describe another thread.
    pub fn wants(&self, open_id: Option<&str>) -> bool {
        self.open
            && (self.stale
                || self
                    .facts
                    .as_ref()
                    .is_none_or(|facts| facts.thread_id.as_deref() != open_id))
    }

    /// Takes the facts a frame worked out. The rows are wrapped again only when they differ.
    pub fn set(&mut self, facts: Facts) {
        #[cfg(test)]
        {
            self.prepares += 1;
        }
        if self.facts.as_ref() != Some(&facts) {
            self.rows = None;
            self.facts = Some(facts);
        }
        self.stale = false;
    }

    /// How the last frame drew the details, while they are open.
    pub fn shown(&self) -> Option<Layout> {
        self.shown.filter(|_| self.open)
    }

    /// Scrolls the text `delta` rows, down for a positive one.
    pub fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
    }

    /// Scrolls the text a page down, or up when `down` is false.
    pub fn page(&mut self, down: bool) {
        let page = self.page.max(1) as isize;
        self.scroll_by(if down { page } else { -page });
    }

    pub fn scroll_home(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_end(&mut self) {
        self.scroll = usize::MAX / 2;
    }

    /// Forgets where the last frame drew the details, for a frame that doesn't draw them.
    pub fn forget_drawn(&mut self) {
        self.drawn = None;
        self.shown = None;
        self.moved = false;
    }

    /// Draws the details as the pane at `area`. `focused` says whether they have the keyboard,
    /// which brightens the border.
    pub fn draw_pane(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        focused: bool,
        environment: &Identity,
        theme: &Theme,
    ) {
        self.draw_card(frame, area, Layout::Pane, focused, environment, theme);
    }

    /// Draws the details as a dialog on `screen`: at the right under the title row, as the
    /// desktop's popover opens under its button, at most `DIALOG_WIDTH` columns wide with a
    /// column of the screen showing on each side, and as tall as its text down to the row
    /// above the status line. On a screen too small for that it takes what there is.
    pub fn draw_dialog(
        &mut self,
        frame: &mut Frame,
        screen: Rect,
        environment: &Identity,
        theme: &Theme,
    ) {
        let width = if screen.width >= BORDER_MIN_WIDTH + 2 {
            (screen.width - 2).min(DIALOG_WIDTH)
        } else {
            screen.width
        };
        let margin = u16::from(width < screen.width);
        let (top, bottom) = if screen.height >= 6 { (1, 1) } else { (0, 0) };
        let room = screen.height - top - bottom;
        let bordered = width >= BORDER_MIN_WIDTH && room >= 3;
        let frame_rows = if bordered { 3 } else { 1 };
        let text_width = if bordered { width - 4 } else { width };
        let wanted = self.lines(text_width as usize, environment, theme).len() as u16 + frame_rows;
        let area = Rect {
            x: screen.right() - width - margin,
            y: screen.y + top,
            width,
            height: wanted.min(room),
        };
        self.draw_card(frame, area, Layout::Dialog, true, environment, theme);
    }

    /// The facts wrapped at `width`, from the last frame when it had the same width.
    fn lines(&mut self, width: usize, environment: &Identity, theme: &Theme) -> &[Line<'static>] {
        if self.rows.as_ref().map(|rows| rows.width) != Some(width) {
            #[cfg(test)]
            {
                self.wraps += 1;
            }
            let lines = wrap_facts(self.facts.as_ref(), width, environment, theme);
            self.rows = Some(Rows { width, lines });
        }
        &self.rows.as_ref().expect("wrapped above").lines
    }

    /// Draws the card at `area`: a title row with × at its end, and the text under it, which
    /// scrolls when it doesn't fit. With room for one, a border goes round it and names where
    /// more text is.
    fn draw_card(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        layout: Layout,
        focused: bool,
        environment: &Identity,
        theme: &Theme,
    ) {
        self.shown = Some(layout);
        self.moved = false;
        let bordered = area.width >= BORDER_MIN_WIDTH && area.height >= 3;
        // A column of space inside each side of the border.
        let inner = if bordered {
            Rect {
                x: area.x + 2,
                y: area.y + 1,
                width: area.width - 4,
                height: area.height - 2,
            }
        } else {
            area
        };
        let width = inner.width as usize;
        let visible = (inner.height as usize).saturating_sub(1);
        let total = self.lines(width, environment, theme).len();
        self.scroll = self.scroll.min(total.saturating_sub(visible));
        self.page = visible;
        let muted = Style::new().fg(theme.muted);

        let mut lines = Vec::with_capacity(visible + 1);
        if inner.height > 0 && width > 0 {
            let close = if width >= 4 { " ×" } else { "" };
            let title = clip("Thread details", width.saturating_sub(close.width()));
            lines.push(row(
                vec![Span::styled(
                    title,
                    Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
                )],
                vec![Span::styled(close, muted)],
                width,
            ));
        }
        let scroll = self.scroll;
        if let Some(rows) = self.rows.as_ref() {
            lines.extend(rows.lines.iter().skip(scroll).take(visible).cloned());
        }

        frame.render_widget(Clear, area);
        let style = Style::new().fg(theme.fg).bg(theme.popover);
        if bordered {
            let border = if focused {
                theme.border_strong
            } else {
                theme.border
            };
            let mut block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(border))
                .style(style);
            let more = match (scroll > 0, scroll + visible < total) {
                (true, true) => Some(" more ↑↓ "),
                (true, false) => Some(" more ↑ "),
                (false, true) => Some(" more ↓ "),
                (false, false) => None,
            };
            if let Some(more) = more.filter(|more| more.width() + 2 <= area.width as usize) {
                block = block.title_bottom(Line::styled(more, muted).right_aligned());
            }
            frame.render_widget(block, area);
        } else {
            frame.render_widget(Block::new().style(style), area);
        }
        frame.render_widget(Paragraph::new(lines).style(style), inner);
        // The × and the blank column on each side of it.
        let close = if width >= 4 {
            Rect::new(inner.right() - 2, inner.y, 3, 1).intersection(area)
        } else {
            Rect::default()
        };
        self.drawn = Some(Drawn {
            layout,
            area,
            close,
        });
    }
}

/// The rows of `facts` at `width`: each label in muted text, its values under it, and a blank
/// row between sections. A value wraps at spaces and inside a word too long for a row, to at
/// most its own number of rows, the last cut with `…`. The environment comes last, fitted to
/// one row as the sidebar fits it, and is left out when discovery gave it neither a name nor
/// an address.
fn wrap_facts(
    facts: Option<&Facts>,
    width: usize,
    environment: &Identity,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let muted = Style::new().fg(theme.muted);
    let value = Style::new().fg(theme.fg);
    let mut lines = Vec::new();
    match facts {
        Some(facts) if facts.thread_id.is_some() => {
            for section in &facts.sections {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                lines.push(Line::styled(clip(section.label, width), muted));
                for piece in &section.pieces {
                    let style = if piece.note { muted } else { value };
                    let mut rows = wrap(&piece.text, width);
                    if rows.len() > piece.rows {
                        rows.truncate(piece.rows);
                        if let Some(last) = rows.last_mut() {
                            last.push('…');
                        }
                    }
                    lines.extend(
                        rows.iter()
                            .map(|text| Line::styled(clip(text, width), style)),
                    );
                }
            }
        }
        _ => lines.push(Line::styled(clip("No thread is open.", width), muted)),
    }
    let spans = environment.spans(width, value, muted);
    if !spans.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled(clip("Environment", width), muted));
        lines.push(Line::from(spans));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Depth;
    use serde_json::json;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    fn project(id: &str, title: &str, root: &str) -> Value {
        json!({"id": id, "title": title, "workspaceRoot": root})
    }

    fn thread(worktree: Value, branch: Value) -> Value {
        json!({"id": "t", "projectId": "p", "title": "Fix it",
            "worktreePath": worktree, "branch": branch})
    }

    /// The facts' rows at `width`, with no environment.
    fn shown(facts: &Facts, width: usize) -> Vec<String> {
        let theme = Theme::new(Depth::TrueColor);
        text(&wrap_facts(
            Some(facts),
            width,
            &Identity::default(),
            &theme,
        ))
    }

    fn facts_of(thread: &Value, projects: &[Value]) -> Facts {
        Facts::of(Some("t"), Record::Found(thread), Projects::Listed(projects))
    }

    #[test]
    fn a_worktree_names_its_folder_and_shows_its_path() {
        let projects = [project("p", "Demo", "/repo/demo")];
        let thread = thread(json!("/repo/.t3/worktrees/feature-a"), json!("feature/a"));
        let rows = shown(&facts_of(&thread, &projects), 40);
        assert_eq!(
            rows,
            [
                "Project",
                "Demo",
                "",
                "Workspace",
                "Worktree · feature-a",
                "/repo/.t3/worktrees/feature-a",
                "",
                "Recorded branch",
                "feature/a",
            ]
        );
    }

    #[test]
    fn a_null_worktree_path_is_the_local_checkout_at_the_projects_folder() {
        let projects = [project("p", "Demo", "C:\\code\\demo\\")];
        let facts = facts_of(&thread(Value::Null, Value::Null), &projects);
        let rows = shown(&facts, 40);
        assert_eq!(rows[4], "Local checkout · demo");
        assert_eq!(rows[5], "C:\\code\\demo\\");
        // A null branch is no branch at all, never main.
        assert_eq!(rows[8], "None recorded");
        assert!(!rows.iter().any(|row| row.contains("main")), "{rows:#?}");
    }

    #[test]
    fn missing_and_malformed_fields_read_unavailable() {
        let projects = [project("p", "Demo", "/repo/demo")];
        for (worktree, branch) in [
            (json!(7), json!(["main"])),
            (json!("   "), json!("")),
            (json!({"path": "/x"}), json!(false)),
        ] {
            let rows = shown(&facts_of(&thread(worktree, branch), &projects), 40);
            assert_eq!(rows[4], "Unavailable", "{rows:#?}");
            assert_eq!(rows[7], "Unavailable", "{rows:#?}");
        }
        // Fields left out entirely, as an old shell's copy might.
        let bare = json!({"id": "t", "projectId": "p"});
        let rows = shown(&facts_of(&bare, &projects), 40);
        assert_eq!(rows[4], "Unavailable");
        assert_eq!(rows[7], "Unavailable");
        // A branch of nothing but controls cleans to nothing.
        let rows = shown(
            &facts_of(&thread(Value::Null, json!("\u{1b}\u{7}")), &projects),
            40,
        );
        assert_eq!(rows[8], "Unavailable");
    }

    #[test]
    fn a_project_the_shell_lacks_is_loading_until_its_snapshot_and_then_unavailable() {
        let thread = thread(Value::Null, json!("dev"));
        let loading = Facts::of(Some("t"), Record::Found(&thread), Projects::Loading);
        let rows = shown(&loading, 40);
        assert_eq!(rows[1], "Loading…");
        assert_eq!(rows[4..6], ["Local checkout", "Loading…"]);
        for projects in [Projects::Listed(&[]), Projects::Failed] {
            let rows = shown(&Facts::of(Some("t"), Record::Found(&thread), projects), 40);
            assert_eq!(rows[1], "Unavailable");
            assert_eq!(rows[4..6], ["Local checkout", "Project folder unavailable"]);
        }
        // A worktree needs no project to name it.
        let worktree = json!({"id": "t", "worktreePath": "/w/x", "branch": null});
        let rows = shown(&facts_of(&worktree, &[]), 40);
        assert_eq!(rows[4], "Worktree · x");
        // A project without a title or a folder.
        let rows = shown(&facts_of(&thread, &[json!({"id": "p", "title": ""})]), 40);
        assert_eq!(rows[1], "Untitled");
        assert_eq!(rows[5], "Project folder unavailable");
    }

    #[test]
    fn a_thread_not_yet_sent_is_loading_and_one_never_sent_is_unavailable() {
        let rows = shown(
            &Facts::of(Some("t"), Record::Loading, Projects::Loading),
            40,
        );
        assert_eq!(
            rows,
            [
                "Project",
                "Loading…",
                "",
                "Workspace",
                "Loading…",
                "",
                "Recorded branch",
                "Loading…"
            ]
        );
        let rows = shown(
            &Facts::of(Some("t"), Record::Unavailable, Projects::Loading),
            40,
        );
        assert_eq!(rows.iter().filter(|row| *row == "Unavailable").count(), 3);
        let theme = Theme::new(Depth::TrueColor);
        let none = Facts::of(None, Record::None, Projects::Loading);
        let rows = text(&wrap_facts(Some(&none), 40, &Identity::default(), &theme));
        assert_eq!(rows, ["No thread is open."]);
    }

    #[test]
    fn folder_names_follow_the_desktop() {
        assert_eq!(folder_name("/repo/.t3/worktrees/feature-a"), "feature-a");
        assert_eq!(folder_name("C:\\code\\project\\feature-b\\"), "feature-b");
        assert_eq!(folder_name("/"), "/");
        assert_eq!(folder_name("relative"), "relative");
        assert_eq!(folder_name("/a/b///"), "b");
        // A path of nothing but controls is still a worktree, with no folder or path to show.
        let projects = [project("p", "Demo", "/r")];
        let rows = shown(
            &facts_of(&thread(json!("\u{1b}\u{7}"), json!("b")), &projects),
            40,
        );
        assert_eq!(rows[3..6], ["Workspace", "Worktree", ""]);
    }

    #[test]
    fn hostile_text_is_cleaned_and_keeps_its_characters_whole() {
        let projects = [project(
            "p",
            "\u{1b}]52;c;cHduZWQ=\u{7}De\u{9b}mo\r\nrepo",
            "/r/\u{1b}[2Jroot",
        )];
        let thread = thread(
            json!("/w/\u{1b}[31mfe\u{301}ature\u{7}/👩\u{200d}💻\n"),
            json!("dev\u{1b}[0m\u{9b}x"),
        );
        let rows = shown(&facts_of(&thread, &projects), 60);
        for row in &rows {
            assert!(!row.chars().any(char::is_control), "{row:?}");
        }
        assert_eq!(rows[1], "]52;c;cHduZWQ=Demo repo");
        assert_eq!(rows[4], "Worktree · 👩\u{200d}💻");
        assert_eq!(rows[5], "/w/[31mfe\u{301}ature/👩\u{200d}💻");
        assert_eq!(rows[8], "dev[0mx");
    }

    #[test]
    fn wide_text_wraps_inside_its_width_and_stops_at_its_rows() {
        let projects = [project("p", &"漢字".repeat(40), "/r")];
        let thread = thread(json!(format!("/{}/y", "x".repeat(400))), json!("b"));
        let facts = facts_of(&thread, &projects);
        for width in [1, 2, 3, 7, 20] {
            let rows = shown(&facts, width);
            for row in &rows {
                assert!(row.width() <= width, "{row:?} at {width}");
            }
        }
        let rows = shown(&facts, 20);
        // The title wraps to three rows, the last ending with …, as does the path to eight.
        assert_eq!(
            rows[1..4].iter().filter(|row| row.contains('漢')).count(),
            3
        );
        assert!(rows[3].ends_with('…'), "{rows:#?}");
        let path: Vec<_> = rows.iter().filter(|row| row.starts_with('x')).collect();
        assert_eq!(path.len(), 7, "{rows:#?}");
        assert!(rows.iter().any(|row| row.ends_with("x…")));
    }

    #[test]
    fn huge_fields_are_bounded_before_they_are_cleaned() {
        let huge = format!("/{}/{}", "a".repeat(3_000_000), "end-folder");
        let projects = [project("p", &"t".repeat(3_000_000), &huge)];
        let thread = thread(Value::Null, json!("b".repeat(3_000_000)));
        let facts = facts_of(&thread, &projects);
        let pieces: Vec<&Piece> = facts.sections.iter().flat_map(|s| &s.pieces).collect();
        for piece in &pieces {
            assert!(piece.text.chars().count() <= PATH_HEAD + PATH_TAIL + 1);
        }
        assert_eq!(
            pieces[0].text.chars().count(),
            NAME + 1,
            "the title ends with …"
        );
        assert_eq!(pieces[1].text, "Local checkout · end-folder");
        assert!(pieces[2].text.starts_with("/aaa"));
        assert!(pieces[2].text.ends_with("aaa/end-folder"));
        assert!(pieces[2].text.contains('…'));
        assert!(shown(&facts, 30).len() <= 3 * 3 + VALUE_ROWS * 3 + PATH_ROWS);
    }

    #[test]
    fn a_huge_path_is_blank_or_not_by_the_ends_it_keeps() {
        let spaces = " ".repeat(3_000_000);
        let ended = format!("{spaces}/repo/.t3/worktrees/feature-a\t \n");
        // Text only in the middle, past what either end keeps.
        let pad = " ".repeat(10_000);
        let middle = format!("{pad}/repo/never-shown{pad}");
        // As a worktree's path, and as the project's folder for a thread without one.
        for worktree in [true, false] {
            let rows_for = |path: &str| {
                let (worktree_path, root) = if worktree {
                    (json!(path), "/r")
                } else {
                    (Value::Null, path)
                };
                let projects = [project("p", "Demo", root)];
                shown(&facts_of(&thread(worktree_path, json!("b")), &projects), 40)
            };
            let (kind, unusable) = if worktree {
                ("Worktree", ["Unavailable", ""])
            } else {
                (
                    "Local checkout",
                    ["Local checkout", "Project folder unavailable"],
                )
            };
            for path in [spaces.as_str(), middle.as_str()] {
                let rows = rows_for(path);
                assert_eq!(rows[4..6], unusable, "{worktree}");
                assert!(!rows.iter().any(|row| row.contains("never")), "{rows:#?}");
            }
            let rows = rows_for(ended.as_str());
            assert_eq!(rows[4], format!("{kind} · feature-a"));
            assert_eq!(rows[5], "… /repo/.t3/worktrees/feature-a");
        }
    }

    #[test]
    fn heads_and_tails_cut_between_whole_characters() {
        assert_eq!(head("abc", 5), ("abc", false));
        assert_eq!(head("abcdef", 3), ("abc", true));
        assert_eq!(head("ae\u{301}b", 3), ("ae\u{301}", true));
        assert_eq!(head("ae\u{301}b", 2), ("a", true));
        assert_eq!(head("", 3), ("", false));
        assert_eq!(tail("abcdef", 3), ("def", true));
        assert_eq!(tail("abc", 3), ("abc", false));
        assert_eq!(tail("xa\u{301}", 3), ("xa\u{301}", false));
        assert_eq!(tail("xya\u{301}", 2), ("a\u{301}", true));
        assert_eq!(tail("", 3), ("", false));
        // A window that starts inside a character drops the part it has.
        let joined = format!("{}e{}", "a".repeat(10), "\u{301}".repeat(30));
        let (end, cut) = tail(&joined, 5);
        assert!(cut);
        assert!(end.is_empty(), "{end:?}");
    }

    #[test]
    fn the_layout_keeps_the_conversation_its_width() {
        let main = |width, height| Rect::new(0, 0, width, height);
        assert_eq!(layout(main(97, 20)), Layout::Pane);
        assert_eq!(layout(main(96, 20)), Layout::Dialog);
        assert_eq!(layout(main(200, 15)), Layout::Pane);
        assert_eq!(layout(main(200, 14)), Layout::Dialog);
        assert_eq!(layout(main(0, 0)), Layout::Dialog);
        assert_eq!(beside(main(100, 20)).width, 63);
        assert_eq!(pane_area(main(100, 20), 2, 19), Rect::new(64, 2, 36, 17));
    }

    #[test]
    fn only_events_that_can_change_the_facts_mark_them() {
        let mut details = Details {
            open: true,
            ..Details::default()
        };
        let projects = [project("p", "Demo", "/r")];
        let record = thread(Value::Null, Value::Null);
        details.set(facts_of(&record, &projects));
        assert!(!details.wants(Some("t")));
        assert!(details.wants(Some("u")), "another thread opened");

        let quiet = [
            (Applied::Event("enrichment".into()), json!({})),
            (
                Applied::Event("thread.updated".into()),
                json!({"thread": {"id": "u"}}),
            ),
            (
                Applied::Event("thread.removed".into()),
                json!({"threadId": "u"}),
            ),
            (
                Applied::Event("project.updated".into()),
                json!({"project": {"id": "q"}}),
            ),
            (Applied::Duplicate, json!({"thread": {"id": "t"}})),
            (Applied::Synchronized, json!({})),
        ];
        for (applied, item) in &quiet {
            details.note_shell(applied, item, Some("t"));
            assert!(!details.wants(Some("t")), "{applied:?} {item}");
        }
        let loud = [
            (Applied::Snapshot, json!({})),
            (
                Applied::Event("thread.updated".into()),
                json!({"thread": {"id": "t"}}),
            ),
            (
                Applied::Event("thread.removed".into()),
                json!({"threadId": "t"}),
            ),
            (
                Applied::Event("project.updated".into()),
                json!({"project": {"id": "p"}}),
            ),
            (
                Applied::Event("project.removed".into()),
                json!({"projectId": "p"}),
            ),
        ];
        for (applied, item) in &loud {
            details.note_shell(applied, item, Some("t"));
            assert!(details.wants(Some("t")), "{applied:?} {item}");
            details.set(facts_of(&record, &projects));
        }
        for applied in [
            Applied::Event("turn-item.updated".into()),
            Applied::Event("run.updated".into()),
            Applied::Duplicate,
        ] {
            details.note_thread(&applied);
            assert!(!details.wants(Some("t")), "{applied:?}");
        }
        for applied in [
            Applied::Snapshot,
            Applied::Synchronized,
            Applied::Event("thread.meta-updated".into()),
        ] {
            details.note_thread(&applied);
            assert!(details.wants(Some("t")), "{applied:?}");
            details.set(facts_of(&record, &projects));
        }
        // Closed details work nothing out, however stale.
        details.mark();
        details.open = false;
        assert!(!details.wants(Some("t")));
    }

    #[test]
    fn the_same_facts_keep_their_rows_and_new_ones_wrap_again() {
        let theme = Theme::new(Depth::TrueColor);
        let identity = Identity::default();
        let mut details = Details::default();
        let projects = [project("p", "Demo", "/r")];
        let record = thread(Value::Null, json!("one"));
        details.set(facts_of(&record, &projects));
        details.lines(30, &identity, &theme);
        details.lines(30, &identity, &theme);
        assert_eq!(details.wraps, 1);
        details.set(facts_of(&record, &projects));
        details.lines(30, &identity, &theme);
        assert_eq!(details.wraps, 1, "the same facts");
        details.lines(31, &identity, &theme);
        assert_eq!(details.wraps, 2, "a new width");
        details.set(facts_of(&thread(Value::Null, json!("two")), &projects));
        let rows = text(details.lines(31, &identity, &theme));
        assert_eq!(details.wraps, 3);
        assert_eq!(rows.last().map(String::as_str), Some("two"));
    }

    #[test]
    fn the_environment_comes_last_when_discovery_named_it() {
        let theme = Theme::new(Depth::TrueColor);
        let identity = Identity::new(Some("Studio\u{1b}[2J"), "http://127.0.0.1:3773");
        let facts = facts_of(&thread(Value::Null, Value::Null), &[]);
        let rows = text(&wrap_facts(Some(&facts), 40, &identity, &theme));
        assert_eq!(
            rows[rows.len() - 2..],
            ["Environment", "Studio[2J · 127.0.0.1:3773"]
        );
        let rows = text(&wrap_facts(Some(&facts), 10, &identity, &theme));
        assert_eq!(rows.last().map(String::as_str), Some("Studio[2J"));
    }
}
