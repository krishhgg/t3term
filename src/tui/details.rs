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
//!
//! Lineage comes last, after the environment: the threads related to the open one, which
//! lineage.rs works out as the desktop's `ThreadRelationshipsPanel` does. ↑ and ↓ choose
//! among its rows, and Enter or a click opens the chosen thread the way the sidebar opens one.
//! It is worked out again only after an event that can change it, as the facts are.
//!
//! A second tab lists the pull requests linked to the thread, as the desktop's panel beside
//! its details does. `pull_requests` reads and lays them out. The facts and Lineage show on
//! the first tab, and each tab keeps its own scroll and choice while the other shows.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::environment::Identity;
use super::lineage::{self, Group, Lineage, Paging, Target};
use super::pull_requests::{Part, PullRequests};
use super::setup::bounded;
use super::tasks::wrap;
use super::theme::Theme;
use super::{Focus, clip, row, with_bg};
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

/// What the card shows under its tabs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Tab {
    /// The thread's project, workspace, branch and environment.
    #[default]
    Thread,
    /// The pull requests linked to the thread.
    PullRequests,
}

const TABS: [Tab; 2] = [Tab::Thread, Tab::PullRequests];
const THREAD_TAB: &str = "Thread";

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

/// The facts and Lineage wrapped at one width.
struct Rows {
    width: usize,
    lines: Vec<Line<'static>>,
    /// The line of each Lineage row that Enter or a click acts on, in order.
    targets: Vec<(usize, Target)>,
}

impl Rows {
    /// The first Lineage row at or below line `top`, by its place among them.
    fn first_from(&self, top: usize) -> Option<usize> {
        self.targets.iter().position(|(line, _)| *line >= top)
    }

    /// The Lineage rows among the `visible` rows of text from line `top`. Each row's line is
    /// below the last one's, so a frame finds them without looking at the rows out of view.
    fn in_view(&self, top: usize, visible: usize) -> &[(usize, Target)] {
        let start = self.targets.partition_point(|(line, _)| *line < top);
        let end = top.saturating_add(visible);
        let rest = &self.targets[start..];
        &rest[..rest.partition_point(|(line, _)| *line < end)]
    }
}

/// How far the text is scrolled: a row from its top, or a number of rows up from where its
/// last row shows. End asks for the end before a frame knows how many rows the text takes at
/// the width it draws, so the keys after it count from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Offset {
    Top(usize),
    End(usize),
}

impl Default for Offset {
    fn default() -> Offset {
        Offset::Top(0)
    }
}

/// The first row in view, scrolled from `top` as little as brings line `line` into the
/// `visible` rows.
fn into_view(top: usize, line: usize, visible: usize) -> usize {
    if line < top {
        line
    } else if visible > 0 && line >= top + visible {
        line + 1 - visible
    } else {
        top
    }
}

/// Where the last frame drew the details, for the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Drawn {
    pub layout: Layout,
    /// The whole card, its border included.
    pub area: Rect,
    /// The × at the right of its title row.
    pub close: Rect,
    /// The name of each tab, in the order of `TABS`.
    pub tabs: [Rect; 2],
    /// The rows under the tabs.
    pub body: Rect,
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
    /// How far the card's text is scrolled. A frame turns it into the first row it shows, kept
    /// within the text.
    scroll: Offset,
    /// The rows of text the last frame showed, which a page scrolls by.
    page: usize,
    /// The furthest the last frame let the text scroll: its rows past the ones it showed. The
    /// keys and the wheel go no further before the next frame, so a scroll back up after one
    /// past the end counts. None until a frame draws the details.
    bottom: Option<usize>,
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
    /// The Lineage section a frame worked out, for the thread it names.
    lineage: Option<Lineage>,
    /// Set when an event may have changed the Lineage section, as `stale` is for the facts.
    lineage_stale: bool,
    /// How much of each Lineage group shows.
    paging: Paging,
    /// The Lineage row ↑ and ↓ chose, which Enter acts on.
    selected: Option<Target>,
    /// Set when the choice moved, so the next frame scrolls its row into view.
    reveal: bool,
    /// Where the last frame drew each Lineage row it showed, for the mouse. Anything that
    /// moves the text clears them, so a click acts on no row until the next frame draws them.
    targets: Vec<(Rect, Target)>,
    /// The tab that shows.
    tab: Tab,
    /// The thread's linked pull requests, for the second tab.
    pub pulls: PullRequests,
    /// The time the last frame drew at, for the pull requests' relative times.
    now: i64,
    /// How many times the facts were worked out and wrapped, so the tests can tell what a
    /// frame or an event did.
    #[cfg(test)]
    pub prepares: usize,
    #[cfg(test)]
    pub wraps: usize,
    /// How many times the Lineage section was worked out.
    #[cfg(test)]
    pub lineage_prepares: usize,
}

impl Details {
    /// Opens or closes the details. They open at the top of their text.
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.scroll = Offset::Top(0);
        self.bottom = None;
        self.selected = None;
        self.shown = None;
        self.moved = true;
        self.shift();
    }

    /// Notes that the screen resized, which can move the details and every row in them.
    pub fn resized(&mut self) {
        self.moved = true;
        self.shift();
    }

    /// Notes that the text moved, or may have, before the next frame draws it. A click then
    /// acts on no Lineage row, since the row the last frame drew there may be gone or
    /// elsewhere. ↑, ↓ and Enter still choose and act, by the order of the rows.
    fn shift(&mut self) {
        self.targets.clear();
    }

    /// Notes that the facts, Lineage and the pull requests may have changed, as a thread's
    /// watch reconnecting or closing can.
    pub fn mark(&mut self) {
        self.note(true, true);
    }

    /// Notes that the facts, Lineage or both may have changed. The pull requests come from the
    /// thread's record, as the facts do, so whatever can change the facts can change them too.
    fn note(&mut self, facts: bool, links: bool) {
        self.stale |= facts;
        self.lineage_stale |= links;
        if facts {
            self.pulls.mark();
        }
        if facts || links {
            self.moved = true;
            self.shift();
        }
    }

    /// Notes what an item from the shell's feed changed: a snapshot, or an event for the open
    /// thread, `open_id`, or for its project. An event for a thread related to the open one,
    /// or one that names it as its parent, changes Lineage. Others, such as an enrichment of a
    /// project's repository or an event for another thread, leave both as they are.
    pub fn note_shell(&mut self, applied: &Applied, item: &Value, open_id: Option<&str>) {
        let names_open = |id: Option<&str>| open_id.is_some_and(|open_id| id == Some(open_id));
        let related = |id: Option<&str>| match (id, self.lineage.as_ref()) {
            (Some(id), Some(lineage)) => lineage.relates(id),
            _ => false,
        };
        let (facts, links) = match applied {
            Applied::Snapshot => (true, true),
            Applied::Event(kind) => match kind.as_str() {
                "thread.updated" => {
                    let thread = &item["thread"];
                    let id = thread.get("id").and_then(Value::as_str);
                    // A thread that names the open one as its parent can be a new fork.
                    let child = names_open(lineage::parent_of(thread));
                    (names_open(id), names_open(id) || child || related(id))
                }
                "thread.removed" => {
                    let id = item.get("threadId").and_then(Value::as_str);
                    (names_open(id), names_open(id) || related(id))
                }
                "project.updated" => (self.names_project(item.pointer("/project/id")), false),
                "project.removed" => (self.names_project(item.get("projectId")), false),
                _ => (false, false),
            },
            _ => (false, false),
        };
        self.note(facts, links);
    }

    /// Notes what an item from the open thread's watch changed: a snapshot, an event for the
    /// thread itself, or the watch saying it caught up, which ends the wait for a thread it
    /// never sent. An agent's or a context transfer's event changes Lineage only. Each piece of
    /// a streamed answer and each run event leave both as they are.
    pub fn note_thread(&mut self, applied: &Applied) {
        let (facts, links) = match applied {
            Applied::Snapshot => (true, true),
            Applied::Synchronized => (true, false),
            Applied::Event(kind) if kind.starts_with("thread.") => (true, true),
            Applied::Event(kind) => {
                let links = kind == "subagent.updated" || kind.starts_with("context-transfer.");
                (false, links)
            }
            _ => (false, false),
        };
        self.note(facts, links);
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
    /// details again, so the last thread's never show for it. Lineage starts over with its
    /// first rows and nothing chosen, and the pull requests at the top of their list.
    pub fn follow(&mut self) {
        self.mark();
        self.scroll = Offset::Top(0);
        self.bottom = None;
        self.paging = Paging::default();
        self.selected = None;
        self.pulls.follow();
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

    /// Whether this frame has to work Lineage out for thread `open_id`, as `wants` says for the
    /// facts.
    pub fn wants_lineage(&self, open_id: Option<&str>) -> bool {
        self.open
            && (self.lineage_stale
                || self
                    .lineage
                    .as_ref()
                    .is_none_or(|lineage| lineage.thread_id() != open_id))
    }

    /// Takes the Lineage section a frame worked out. The rows are wrapped again only when it
    /// differs.
    pub fn set_lineage(&mut self, lineage: Lineage) {
        #[cfg(test)]
        {
            self.lineage_prepares += 1;
        }
        if self.lineage.as_ref() != Some(&lineage) {
            self.rows = None;
            self.lineage = Some(lineage);
            self.keep_choice();
        }
        self.lineage_stale = false;
    }

    /// Keeps the chosen row while it shows. When Show more goes because its group shows all,
    /// the group's last row is chosen. Any other row that went leaves nothing chosen.
    fn keep_choice(&mut self) {
        let Some(lineage) = self.lineage_shown() else {
            self.selected = None;
            return;
        };
        self.selected = match self.selected.as_ref() {
            Some(target) if lineage.has(target) => Some(target.clone()),
            Some(Target::More(group)) => lineage
                .last_row(*group)
                .map(|row| Target::Thread(row.id.clone())),
            _ => None,
        };
    }

    /// The Lineage section, when it shows: it describes the thread the facts do, and it has
    /// rows or running agents.
    fn lineage_shown(&self) -> Option<&Lineage> {
        let thread_id = self.facts.as_ref()?.thread_id.as_deref()?;
        self.lineage
            .as_ref()
            .filter(|lineage| lineage.thread_id() == Some(thread_id) && lineage.shows())
    }

    /// The Lineage rows Enter or a click acts on, in order.
    fn lineage_targets(&self) -> Vec<Target> {
        self.lineage_shown()
            .map(Lineage::targets)
            .unwrap_or_default()
    }

    /// Where the chosen row is among `targets`.
    fn chosen(&self, targets: &[Target]) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        targets.iter().position(|target| target == selected)
    }

    pub fn paging(&self) -> &Paging {
        &self.paging
    }

    /// The Lineage row ↑ and ↓ chose, for the tests. Enter takes it through `enter`.
    #[cfg(test)]
    pub fn selected(&self) -> Option<&Target> {
        self.selected.as_ref()
    }

    /// Chooses a Lineage row, as a click on it does.
    pub fn choose(&mut self, target: Target) {
        self.selected = Some(target);
    }

    /// Whether thread `id` has a Lineage row that shows and can open.
    pub fn can_open(&self, id: &str) -> bool {
        self.lineage_shown()
            .and_then(|lineage| lineage.row(id))
            .is_some_and(|row| row.available)
    }

    /// The Lineage row the last frame drew at column `x` of screen row `y`.
    pub fn target_at(&self, x: u16, y: u16) -> Option<Target> {
        self.targets
            .iter()
            .find(|(area, _)| y == area.y && x >= area.x && x < area.right())
            .map(|(_, target)| target.clone())
    }

    /// Shows up to `lineage::PAGE` more rows of `group`.
    pub fn show_more(&mut self, group: Group) {
        self.paging.show_more(group);
        self.note(false, true);
        self.reveal = true;
    }

    /// Opens or closes Previous agents.
    pub fn toggle_previous(&mut self) {
        self.paging.toggle_previous();
        self.note(false, true);
        self.reveal = true;
    }

    /// Chooses the next Lineage row, or with none chosen the first at or below the top of the
    /// text. Past the last one, or without Lineage rows, it scrolls the text down a row. On the
    /// pull requests' tab it selects the next one, or in the selected one's details chooses
    /// the next control in view or scrolls.
    pub fn down(&mut self) {
        if self.tab == Tab::PullRequests {
            self.pulls.step(true);
            return;
        }
        let targets = self.lineage_targets();
        let next = match (self.chosen(&targets), self.rows.as_ref()) {
            (Some(at), _) => Some(at + 1),
            (None, Some(rows)) => rows.first_from(self.top()),
            (None, None) => Some(0),
        };
        match next.and_then(|at| targets.get(at)) {
            Some(target) => self.step_to(target.clone()),
            None => self.scroll_by(1),
        }
    }

    /// Chooses the Lineage row before the chosen one. From the first one, or with none chosen,
    /// it leaves none chosen and scrolls the text up a row. On the pull requests' tab it
    /// selects the one before, or in the selected one's details chooses the control before in
    /// view or scrolls.
    pub fn up(&mut self) {
        if self.tab == Tab::PullRequests {
            self.pulls.step(false);
            return;
        }
        let targets = self.lineage_targets();
        match self.chosen(&targets) {
            Some(at) if at > 0 => self.step_to(targets[at - 1].clone()),
            _ => {
                self.selected = None;
                self.scroll_by(-1);
            }
        }
    }

    /// Chooses `target` for ↑ or ↓, and scrolls its row into view where the last frame drew
    /// it, so a key after it scrolls from there. The next frame scrolls it into view again
    /// where it draws it.
    fn step_to(&mut self, target: Target) {
        self.selected = Some(target);
        self.reveal = true;
        if let Some(line) = self.selected_line().filter(|_| self.bottom.is_some()) {
            let top = self.top();
            let view = into_view(top, line, self.page);
            if view != top {
                self.scroll = Offset::Top(view);
            }
        }
        self.shift();
    }

    /// The first row of text in view, as the last frame laid it out.
    fn top(&self) -> usize {
        match self.scroll {
            Offset::Top(row) => row,
            Offset::End(up) => self.bottom.unwrap_or_default().saturating_sub(up),
        }
    }

    /// What the status line says Enter does: None when Lineage has no rows to choose, nothing
    /// while none is chosen, and else what Enter does with the chosen row.
    pub fn lineage_keys(&self) -> Option<String> {
        let lineage = self.lineage_shown()?;
        if !lineage.has_targets() {
            return None;
        }
        let Some(selected) = self.selected.as_ref() else {
            return Some(String::new());
        };
        Some(match selected {
            Target::Thread(id) => match lineage.row(id) {
                Some(row) if row.available => {
                    format!("Enter open {}", row.relation.name().to_lowercase())
                }
                Some(row) => format!("{} unavailable", row.relation.name()),
                None => String::new(),
            },
            Target::More(group) => format!("Enter show {} more", lineage.more(*group)),
            Target::Previous if self.paging.previous_open() => "Enter hide previous agents".into(),
            Target::Previous => "Enter show previous agents".into(),
        })
    }

    /// How the last frame drew the details, while they are open.
    pub fn shown(&self) -> Option<Layout> {
        self.shown.filter(|_| self.open)
    }

    /// The tab that shows, for the tests.
    #[cfg(test)]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// Scrolls the thread's text `delta` rows, down for a positive one, no further than the
    /// last frame let it.
    pub fn scroll_by(&mut self, delta: isize) {
        let bottom = self.bottom;
        let within = |rows: usize| bottom.map_or(rows, |bottom| rows.min(bottom));
        self.scroll = match self.scroll {
            Offset::Top(row) => Offset::Top(within(row.saturating_add_signed(delta))),
            Offset::End(up) => {
                // Down leaves fewer rows under the text in view, and up more.
                let up = up.saturating_add_signed(delta.saturating_neg());
                Offset::End(within(up))
            }
        };
        self.shift();
    }

    /// Shows `tab`. Each tab keeps its own scroll and choice while the other shows. Until a
    /// frame draws the tab, a click acts on none of what the last frame drew.
    pub fn show(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.moved = true;
            self.shift();
        }
    }

    /// Whether this frame has to work the pull requests out for thread `open_id`, on an
    /// environment that links them to threads when `supported` is true.
    pub fn wants_pulls(&self, open_id: Option<&str>, supported: bool) -> bool {
        self.open && self.pulls.wants(open_id, supported)
    }

    /// Takes part of what a host said of the pull request whose details asked with `ticket`,
    /// or why the read failed. The details lay out again when it is the read they wait for.
    pub fn answer_pull(&mut self, ticket: u64, part: Part) {
        if self.pulls.answer(ticket, part) {
            self.moved = true;
        }
    }

    /// Enter on the thread's tab hands back the Lineage row ↑ and ↓ chose, for the caller to
    /// act on. On the pull requests' tab it does what the control ↑ and ↓ chose in a pull
    /// request's details does, or else shows the selected one's details in the list's place,
    /// or the list again.
    pub fn enter(&mut self) -> Option<Target> {
        match self.tab {
            Tab::Thread => self.selected.clone(),
            Tab::PullRequests => {
                self.pulls.enter();
                self.moved = true;
                None
            }
        }
    }

    /// Goes back a step: from a pull request's details to the list, or from the pull requests
    /// to the thread's tab.
    pub fn back(&mut self) {
        if self.tab == Tab::PullRequests && self.pulls.back() {
            self.moved = true;
        } else {
            self.show(Tab::Thread);
        }
    }

    /// The keys the status line offers while the details have the keyboard, for the tab and
    /// what it shows: the fullest first, and each after it shorter. The status line shows the
    /// first that fits, so Esc shows wherever it has room. `modal` says whether the details
    /// show as a dialog, which Esc closes, rather than a pane, which Esc leaves. With Lineage
    /// rows to choose on the thread's tab, ↑ and ↓ choose among them, and what Enter does with
    /// the chosen row leads. So does what Enter does with the control chosen in a pull
    /// request's details.
    pub fn keys(&self, modal: bool) -> Vec<String> {
        let lineage = match self.tab {
            Tab::Thread => self.lineage_keys(),
            Tab::PullRequests => {
                let action = self.pulls.action().filter(|_| self.pulls.viewing());
                if let Some(action) = action {
                    let plain = self.plain_keys(modal);
                    let mut keys: Vec<String> = plain
                        .iter()
                        .map(|keys| format!("Enter {action} · {keys}"))
                        .collect();
                    // Esc still shows where what Enter does has no room.
                    keys.extend(plain.last().map(|keys| keys.to_string()));
                    return keys;
                }
                None
            }
        };
        let Some(action) = lineage else {
            let plain = self.plain_keys(modal);
            return plain.iter().map(|keys| keys.to_string()).collect();
        };
        let shapes = if modal {
            [
                "↑↓ choose · PgUp scroll · Esc close · → pull requests",
                "↑↓ choose · PgUp scroll · Esc close · → PRs",
                "↑↓ choose · Esc close · → PRs",
                "Esc close · → PRs",
                "Esc close",
            ]
        } else {
            [
                "↑↓ choose · PgUp scroll · Esc leave · Alt+B close · → pull requests · Tab focus",
                "↑↓ choose · PgUp scroll · Esc leave · Alt+B close · → PRs",
                "↑↓ choose · Esc leave · → PRs",
                "Esc leave · → PRs",
                "Esc leave",
            ]
        };
        if action.is_empty() {
            return shapes.iter().map(|keys| keys.to_string()).collect();
        }
        let mut keys: Vec<String> = shapes
            .iter()
            .map(|keys| format!("{action} · {keys}"))
            .collect();
        // Esc still shows where what Enter does has no room.
        keys.push(shapes[4].to_string());
        keys
    }

    /// The keys of `keys` for a tab with no Lineage rows to choose.
    fn plain_keys(&self, modal: bool) -> &'static [&'static str] {
        match (self.tab, self.pulls.viewing(), modal) {
            (Tab::Thread, _, true) => &[
                "↑↓/PgUp scroll · Esc close · → pull requests",
                "↑↓/PgUp scroll · Esc close · → PRs",
                "↑↓ scroll · Esc close · → PRs",
                "Esc close · → PRs",
                "Esc close",
            ],
            (Tab::Thread, _, false) => &[
                "↑↓/PgUp scroll · Esc leave · Alt+B close · → pull requests · Tab focus",
                "↑↓/PgUp scroll · Esc leave · Alt+B close · → PRs",
                "↑↓ scroll · Esc leave · → PRs",
                "Esc leave · → PRs",
                "Esc leave",
            ],
            (Tab::PullRequests, false, true) => &[
                "↑↓ select · Enter details · ← thread · Esc close",
                "↑↓ · Enter details · ← thread · Esc close",
                "↑↓ · Enter · ← thread · Esc close",
                "Esc close · ← thread",
                "Esc close",
            ],
            (Tab::PullRequests, false, false) => &[
                "↑↓ select · Enter details · ← thread · Esc leave · Alt+B close · Tab focus",
                "↑↓ · Enter details · ← thread · Esc leave · Alt+B close",
                "↑↓ · Enter · ← thread · Esc leave",
                "Esc leave · ← thread",
                "Esc leave",
            ],
            (Tab::PullRequests, true, true) => &[
                "↑↓/PgUp scroll · ← list · Esc close",
                "↑↓ scroll · ← list · Esc close",
                "Esc close · ← list",
                "Esc close",
            ],
            (Tab::PullRequests, true, false) => &[
                "↑↓/PgUp scroll · ← list · Esc leave · Alt+B close · Tab focus",
                "↑↓ scroll · ← list · Esc leave · Alt+B close",
                "Esc leave · ← list",
                "Esc leave",
            ],
        }
    }

    /// A click at `column`, `row`, inside the card the last frame drew: on a tab's name it shows
    /// that tab, and on a pull request it selects it, or shows its details when it is selected
    /// already. On the row above those details it shows the list again. Says whether it changed
    /// what shows. A click on a Lineage row is the caller's, by `target_at`.
    pub fn click(&mut self, column: u16, row: u16) -> bool {
        let Some(drawn) = self.drawn else {
            return false;
        };
        let at = Position::new(column, row);
        if let Some(index) = drawn.tabs.iter().position(|tab| tab.contains(at)) {
            let tab = TABS[index];
            let changed = self.tab != tab;
            self.show(tab);
            return changed;
        }
        if self.tab != Tab::PullRequests || !drawn.body.contains(at) {
            return false;
        }
        let changed = self.pulls.click(usize::from(row - drawn.body.y));
        if changed {
            self.moved = true;
        }
        changed
    }

    /// Scrolls the text `delta` rows for the wheel, which leaves no Lineage row chosen. On the
    /// pull requests' tab it moves the selection `delta` links, or scrolls the selected one's
    /// details while they show.
    pub fn wheel(&mut self, delta: isize) {
        match self.tab {
            Tab::Thread => {
                self.selected = None;
                self.scroll_by(delta);
            }
            Tab::PullRequests => self.pulls.move_by(delta),
        }
    }

    /// Scrolls the text a page down, or up when `down` is false. Like the other keys that
    /// scroll the text, it leaves no Lineage row chosen. On the pull requests' tab it moves the
    /// selection a page, or scrolls the selected one's details a page.
    pub fn page(&mut self, down: bool) {
        match self.tab {
            Tab::Thread => {
                let page = self.page.max(1) as isize;
                self.wheel(if down { page } else { -page });
            }
            Tab::PullRequests => self.pulls.page_by(down),
        }
    }

    pub fn scroll_home(&mut self) {
        match self.tab {
            Tab::Thread => {
                self.selected = None;
                self.scroll = Offset::Top(0);
                self.shift();
            }
            Tab::PullRequests => self.pulls.home(),
        }
    }

    /// Scrolls the text until its last row shows, however many rows the next frame finds it
    /// takes. On the pull requests' tab it selects the last one, or scrolls the selected one's
    /// details to their end.
    pub fn scroll_end(&mut self) {
        match self.tab {
            Tab::Thread => {
                self.selected = None;
                self.scroll = Offset::End(0);
                self.shift();
            }
            Tab::PullRequests => self.pulls.end(),
        }
    }

    /// Forgets where the last frame drew the details, for a frame that doesn't draw them.
    pub fn forget_drawn(&mut self) {
        self.drawn = None;
        self.shown = None;
        self.moved = false;
        self.bottom = None;
        self.targets.clear();
    }

    /// Draws the details as the pane at `area`, at time `now`. `focused` says whether they have
    /// the keyboard, which brightens the border.
    pub fn draw_pane(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        focused: bool,
        now: i64,
        environment: &Identity,
        theme: &Theme,
    ) {
        self.now = now;
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
        now: i64,
        environment: &Identity,
        theme: &Theme,
    ) {
        self.now = now;
        let width = if screen.width >= BORDER_MIN_WIDTH + 2 {
            (screen.width - 2).min(DIALOG_WIDTH)
        } else {
            screen.width
        };
        let margin = u16::from(width < screen.width);
        let (top, bottom) = if screen.height >= 6 { (1, 1) } else { (0, 0) };
        let room = screen.height - top - bottom;
        let bordered = width >= BORDER_MIN_WIDTH && room >= 3;
        // The border's rows, the title row and the tabs' row.
        let frame_rows = if bordered { 4 } else { 2 };
        let text_width = usize::from(if bordered { width - 4 } else { width });
        let text_rows = match self.tab {
            Tab::Thread => self.lines(text_width, environment, theme).len(),
            Tab::PullRequests => self.pulls.wanted(text_width, now, theme),
        };
        let wanted = (text_rows + frame_rows).min(usize::from(room));
        let area = Rect {
            x: screen.right() - width - margin,
            y: screen.y + top,
            width,
            height: wanted as u16,
        };
        self.draw_card(frame, area, Layout::Dialog, true, environment, theme);
    }

    /// The facts and Lineage wrapped at `width`, from the last frame when it had the same
    /// width.
    fn lines(&mut self, width: usize, environment: &Identity, theme: &Theme) -> &[Line<'static>] {
        if self.rows.as_ref().map(|rows| rows.width) != Some(width) {
            #[cfg(test)]
            {
                self.wraps += 1;
            }
            let mut lines = wrap_facts(self.facts.as_ref(), width, environment, theme);
            let mut targets = Vec::new();
            if let Some(lineage) = self.lineage_shown().filter(|_| width > 0) {
                lines.push(Line::default());
                lineage.lines(width, theme, &mut lines, &mut targets);
            }
            self.rows = Some(Rows {
                width,
                lines,
                targets,
            });
        }
        &self.rows.as_ref().expect("wrapped above").lines
    }

    /// The line of the chosen Lineage row in the wrapped text.
    fn selected_line(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        let rows = self.rows.as_ref()?;
        rows.targets
            .iter()
            .find(|(_, target)| target == selected)
            .map(|(line, _)| *line)
    }

    /// Draws the card at `area`: a title row with × at its end, the tabs' row, and the tab's
    /// text under them, which scrolls when it doesn't fit. With room for one, a border goes
    /// round it and names where more text is.
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
        // The title row and the tabs' row come first.
        let visible = (inner.height as usize).saturating_sub(2);
        let muted = Style::new().fg(theme.muted);

        let mut lines = Vec::with_capacity(visible + 2);
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
        let mut tabs = [Rect::default(); 2];
        if inner.height > 1 && width > 0 {
            let (line, places) = tabs_row(self.tab, self.pulls.count(), width, theme);
            lines.push(line);
            for (rect, (start, columns)) in tabs.iter_mut().zip(places) {
                let tab = Rect::new(inner.x + start as u16, inner.y + 1, columns as u16, 1);
                *rect = tab.intersection(inner);
            }
        }
        // Only the thread's tab draws Lineage rows for the mouse.
        self.targets.clear();
        let (above, below) = match self.tab {
            Tab::Thread => {
                let total = self.lines(width, environment, theme).len();
                let bottom = total.saturating_sub(visible);
                let mut scroll = match self.scroll {
                    Offset::Top(row) => row,
                    Offset::End(up) => bottom - up.min(bottom),
                };
                // A row ↑ or ↓ chose, or Show more added rows above, comes into view. Only then
                // is the chosen row looked for among all of them.
                let reveal = std::mem::take(&mut self.reveal);
                if reveal && let Some(line) = self.selected_line() {
                    scroll = into_view(scroll, line, visible);
                }
                let scroll = scroll.min(bottom);
                self.scroll = Offset::Top(scroll);
                self.bottom = Some(bottom);
                self.page = visible;
                if let Some(rows) = self.rows.as_ref() {
                    let in_view = rows.in_view(scroll, visible);
                    // The chosen row shows highlighted while the details have the keyboard.
                    let chosen = self
                        .selected
                        .as_ref()
                        .filter(|_| focused)
                        .and_then(|selected| in_view.iter().find(|(_, target)| target == selected))
                        .map(|(line, _)| *line);
                    for (index, line) in rows.lines.iter().enumerate().skip(scroll).take(visible) {
                        if Some(index) == chosen {
                            lines.push(Line::from(with_bg(line.spans.clone(), theme.highlight)));
                        } else {
                            lines.push(line.clone());
                        }
                    }
                    // Under the title row and the tabs' row.
                    for (line, target) in in_view {
                        let y = inner.y + 2 + (line - scroll) as u16;
                        let area = Rect::new(inner.x, y, inner.width, 1);
                        self.targets.push((area, target.clone()));
                    }
                }
                (scroll > 0, scroll + visible < total)
            }
            Tab::PullRequests => {
                let page = self.pulls.page(width, visible, self.now, focused, theme);
                lines.extend(page.lines.iter().cloned());
                (page.above, page.below)
            }
        };

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
            let more = match (above, below) {
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
        let body = if visible > 0 {
            Rect::new(inner.x, inner.y + 2, inner.width, visible as u16)
        } else {
            Rect::default()
        };
        self.drawn = Some(Drawn {
            layout,
            area,
            close,
            tabs,
            body,
        });
    }
}

/// The tabs' row in `width` columns: each tab's name, the one that shows `tab` in the accent
/// color and underlined, with the number of linked pull requests once they are known and
/// there are some. Says where each name starts and how many columns it takes.
fn tabs_row(
    tab: Tab,
    count: Option<usize>,
    width: usize,
    theme: &Theme,
) -> (Line<'static>, [(usize, usize); 2]) {
    let count = count.filter(|&count| count > 0);
    let long = match count {
        Some(count) => format!("Pull requests {count}"),
        None => "Pull requests".to_string(),
    };
    let pulls = if THREAD_TAB.len() + 2 + long.width() <= width {
        long
    } else {
        match count {
            Some(count) => format!("PRs {count}"),
            None => "PRs".to_string(),
        }
    };
    let active = Style::new()
        .fg(theme.primary)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let muted = Style::new().fg(theme.muted);
    let names = [THREAD_TAB.to_string(), pulls];
    let mut spans = Vec::new();
    let mut places = [(0, 0); 2];
    let mut used = 0;
    for (index, (shown, name)) in TABS.into_iter().zip(names).enumerate() {
        let gap = if index > 0 { 2 } else { 0 };
        if used + gap >= width {
            break;
        }
        let name = clip(&name, width - used - gap);
        let columns = name.width();
        let style = if shown == tab { active } else { muted };
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(name, style));
        places[index] = (used + gap, columns);
        used += gap + columns;
    }
    (Line::from(spans), places)
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

    fn event(kind: &str) -> Applied {
        Applied::Event(kind.into())
    }

    /// `row` with its runs of spaces made one.
    fn squeezed(row: &str) -> String {
        row.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn only_events_that_can_change_lineage_mark_it() {
        let mut details = Details {
            open: true,
            ..Details::default()
        };
        let projects = [project("p", "Demo", "/r")];
        let record = thread(Value::Null, Value::Null);
        details.set(facts_of(&record, &projects));
        let fork = json!({"id": "f", "lineage": {"parentThreadId": "t"}});
        let threads = [record.clone(), fork];
        let paging = Paging::default();
        details.set_lineage(Lineage::of(Some("t"), &threads, None, false, &paging));
        assert!(!details.wants_lineage(Some("t")));
        assert!(details.wants_lineage(Some("u")), "another thread opened");

        let quiet = [
            (event("thread.updated"), json!({"thread": {"id": "u"}})),
            (event("thread.removed"), json!({"threadId": "u"})),
            (event("project.updated"), json!({"project": {"id": "p"}})),
            (event("enrichment"), json!({})),
            (Applied::Synchronized, json!({})),
        ];
        for (applied, item) in &quiet {
            details.note_shell(applied, item, Some("t"));
            assert!(!details.wants_lineage(Some("t")), "{applied:?} {item}");
        }
        // A new fork or agent names t as its parent, by its lineage or the run it came from.
        let child = json!({"thread": {"id": "g", "lineage": {"parentThreadId": "t"}}});
        let forked = json!({"thread": {"id": "g", "forkedFrom": {"type": "run", "threadId": "t"}}});
        let loud = [
            (Applied::Snapshot, json!({})),
            (event("thread.updated"), json!({"thread": {"id": "t"}})),
            (event("thread.updated"), json!({"thread": {"id": "f"}})),
            (event("thread.updated"), child),
            (event("thread.updated"), forked),
            (event("thread.removed"), json!({"threadId": "f"})),
        ];
        for (applied, item) in &loud {
            details.note_shell(applied, item, Some("t"));
            assert!(details.wants_lineage(Some("t")), "{applied:?} {item}");
            details.set_lineage(Lineage::of(Some("t"), &threads, None, false, &paging));
        }
        for applied in [
            event("turn-item.updated"),
            event("run.updated"),
            Applied::Synchronized,
            Applied::Duplicate,
        ] {
            details.note_thread(&applied);
            assert!(!details.wants_lineage(Some("t")), "{applied:?}");
        }
        details.set(facts_of(&record, &projects));
        for applied in [
            event("subagent.updated"),
            event("context-transfer.created"),
            event("context-transfer.updated"),
        ] {
            details.note_thread(&applied);
            assert!(details.wants_lineage(Some("t")), "{applied:?}");
            assert!(!details.wants(Some("t")), "the facts stay: {applied:?}");
            details.set_lineage(Lineage::of(Some("t"), &threads, None, false, &paging));
        }
        for applied in [Applied::Snapshot, event("thread.meta-updated")] {
            details.note_thread(&applied);
            assert!(details.wants_lineage(Some("t")), "{applied:?}");
            details.set_lineage(Lineage::of(Some("t"), &threads, None, false, &paging));
        }
        // Show more and Previous agents work it out again. Closed details don't.
        details.toggle_previous();
        assert!(details.wants_lineage(Some("t")));
        details.open = false;
        assert!(!details.wants_lineage(Some("t")));
    }

    #[test]
    fn the_arrows_choose_lineage_rows_and_the_keys_say_what_enter_does() {
        let theme = Theme::new(Depth::TrueColor);
        let identity = Identity::default();
        let mut details = Details {
            open: true,
            ..Details::default()
        };
        let record = thread(Value::Null, Value::Null);
        details.set(facts_of(&record, &[]));
        details.lines(30, &identity, &theme);
        // Without Lineage the arrows scroll, as they always did.
        details.down();
        assert_eq!((details.selected(), details.top()), (None, 1));
        assert_eq!(details.lineage_keys(), None);
        details.up();
        assert_eq!(details.top(), 0);

        let mut lead = record.clone();
        lead["lineage"] = json!({"parentThreadId": "gone"});
        let fork = json!({"id": "f", "title": "Fork", "lineage": {"parentThreadId": "t"}});
        let threads = [lead, fork];
        let lineage = Lineage::of(Some("t"), &threads, None, false, &Paging::default());
        details.set_lineage(lineage);
        let rows = text(details.lines(30, &identity, &theme));
        let at = rows.iter().position(|row| row == "Lineage").unwrap();
        assert_eq!(rows[at - 1], "", "a blank row before it");
        assert_eq!(squeezed(&rows[at + 1]), "↑ gone Unavailable");
        assert_eq!(squeezed(&rows[at + 2]), "↳ Fork Unknown");
        assert_eq!(rows.len(), at + 3);
        assert_eq!(details.lineage_keys().as_deref(), Some(""));

        details.down();
        assert_eq!(details.selected(), Some(&Target::Thread("gone".into())));
        let keys = details.lineage_keys();
        assert_eq!(keys.as_deref(), Some("Parent thread unavailable"));
        assert!(!details.can_open("gone"));
        details.down();
        assert_eq!(details.lineage_keys().as_deref(), Some("Enter open fork"));
        assert!(details.can_open("f"));
        // Past the last row, the text scrolls and the choice stays.
        let scroll = details.top();
        details.down();
        assert_eq!(details.selected(), Some(&Target::Thread("f".into())));
        assert_eq!(details.top(), scroll + 1);
        details.up();
        assert_eq!(details.selected(), Some(&Target::Thread("gone".into())));
        // Up from the first row leaves none chosen.
        details.up();
        assert_eq!(details.selected(), None);
        // The wheel and the page keys leave none chosen too.
        details.down();
        details.wheel(1);
        assert_eq!(details.selected(), None);
        details.down();
        assert!(details.selected().is_some());
        details.page(true);
        assert_eq!(details.selected(), None);
        // Another thread starts with none chosen.
        details.down();
        details.follow();
        assert_eq!(details.selected(), None);
    }

    #[test]
    fn show_more_hands_its_choice_to_the_last_row_it_adds() {
        let theme = Theme::new(Depth::TrueColor);
        let identity = Identity::default();
        let mut details = Details {
            open: true,
            ..Details::default()
        };
        let record = thread(Value::Null, Value::Null);
        details.set(facts_of(&record, &[]));
        let mut threads = vec![record];
        for i in 0..8 {
            let fork = json!({"id": format!("f{i}"), "lineage": {"parentThreadId": "t"}});
            threads.push(fork);
        }
        let lineage = |details: &Details| {
            let paging = details.paging();
            Lineage::of(Some("t"), &threads, None, false, paging)
        };
        details.set_lineage(lineage(&details));
        details.lines(30, &identity, &theme);
        for _ in 0..7 {
            details.down();
        }
        assert_eq!(details.selected(), Some(&Target::More(Group::Related)));
        let keys = details.lineage_keys();
        assert_eq!(keys.as_deref(), Some("Enter show 2 more"));
        details.show_more(Group::Related);
        assert!(details.wants_lineage(Some("t")));
        details.set_lineage(lineage(&details));
        // All eight show, by id as none has a time, so f7 comes last.
        assert_eq!(details.selected(), Some(&Target::Thread("f7".into())));
        assert_eq!(details.lineage_prepares, 2);
    }

    /// Draws `details` as a pane with the keyboard at `area` of a screen 40 columns by 30 rows.
    /// For each row of the pane, returns its text and whether its first column of text shows
    /// highlighted.
    fn draw_at(details: &mut Details, area: Rect) -> Vec<(String, bool)> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let theme = Theme::new(Depth::TrueColor);
        let identity = Identity::default();
        let mut terminal = Terminal::new(TestBackend::new(40, 30)).unwrap();
        let frame = terminal
            .draw(|frame| details.draw_pane(frame, area, true, 0, &identity, &theme))
            .unwrap();
        let buffer = frame.buffer.clone();
        (area.y..area.bottom())
            .map(|y| {
                let text = (area.x..area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>();
                (text, buffer[(area.x + 2, y)].bg == theme.highlight)
            })
            .collect()
    }

    #[test]
    fn a_frame_maps_the_lineage_rows_in_view_and_no_others() {
        let theme = Theme::new(Depth::TrueColor);
        let identity = Identity::default();
        let mut details = Details {
            open: true,
            ..Details::default()
        };
        let record = thread(Value::Null, Value::Null);
        details.set(facts_of(&record, &[]));
        let mut threads = vec![record];
        for i in 0..40 {
            let (id, title) = (format!("f{i:02}"), format!("Fork {i:02}"));
            let fork = json!({"id": id, "title": title, "lineage": {"parentThreadId": "t"}});
            threads.push(fork);
        }
        // Two pages past the first show 30 of the 40 forks, and a Show more row under them.
        details.show_more(Group::Related);
        details.show_more(Group::Related);
        let lineage = Lineage::of(Some("t"), &threads, None, false, details.paging());
        details.set_lineage(lineage);
        // The pane's 18 rows have room for 14 rows of text under the title row and the tabs'
        // row.
        let pane = Rect::new(2, 1, 36, 18);
        let rows = text(details.lines(32, &identity, &theme));
        let heading = rows.iter().position(|row| row == "Lineage").unwrap();
        // The rows come in the order of their lines, which a frame counts on to find them.
        let targets = &details.rows.as_ref().unwrap().targets;
        assert_eq!(targets.len(), 31);
        assert!(targets.windows(2).all(|pair| pair[0].0 < pair[1].0));

        // Each row of the pane that shows a Lineage row has the row's target, and no other row
        // has one, on the screen or off it.
        let check = |details: &Details, drawn: &[(String, bool)]| -> Vec<Target> {
            let mut mapped = Vec::new();
            for (y, (text, _)) in (pane.y..).zip(drawn) {
                let shown = text.trim_matches(|c: char| c == '│' || c == ' ');
                let expected = if let Some(rest) = shown.strip_prefix("↳ Fork ") {
                    Some(Target::Thread(format!("f{}", &rest[..2])))
                } else if shown.starts_with("+ Show") {
                    Some(Target::More(Group::Related))
                } else {
                    None
                };
                assert_eq!(details.target_at(pane.x + 2, y), expected, "{shown:?}");
                mapped.extend(expected);
            }
            assert_eq!(details.targets.len(), mapped.len());
            mapped
        };
        let forks = |range: std::ops::Range<usize>| -> Vec<Target> {
            range.map(|i| Target::Thread(format!("f{i:02}"))).collect()
        };

        // Scrolled to the Lineage heading, which no row acts on, the pane shows it and the 13
        // forks under it. The fork after them, a row below the pane's last, has no target.
        details.scroll_by(heading as isize);
        let drawn = draw_at(&mut details, pane);
        assert_eq!(check(&details, &drawn), forks(0..13));
        // A row further, the first fork is the pane's first row of text.
        details.scroll_by(1);
        let drawn = draw_at(&mut details, pane);
        assert_eq!(check(&details, &drawn), forks(0..14));
        // At the end, Show more is the pane's last row.
        details.scroll_end();
        let drawn = draw_at(&mut details, pane);
        let mut end = forks(17..30);
        end.push(Target::More(Group::Related));
        assert_eq!(check(&details, &drawn), end);

        // A chosen row in view shows highlighted. One out of view leaves every row as it was.
        details.choose(Target::Thread("f20".into()));
        let drawn = draw_at(&mut details, pane);
        let lit: Vec<_> = drawn.iter().filter(|row| row.1).collect();
        assert_eq!(lit.len(), 1, "{drawn:?}");
        assert!(lit[0].0.contains("↳ Fork 20"), "{lit:?}");
        details.choose(Target::Thread("f00".into()));
        let drawn = draw_at(&mut details, pane);
        assert!(drawn.iter().all(|row| !row.1), "{drawn:?}");
        assert_eq!(check(&details, &drawn), end);

        // A pane five rows high has room inside its border for one row of text under the title
        // row and the tabs' row. One two rows high has no border and room for those two rows
        // only, and one a row high or none has room for less.
        for (height, in_view) in [
            (5, vec![Target::More(Group::Related)]),
            (2, Vec::new()),
            (1, Vec::new()),
            (0, Vec::new()),
        ] {
            details.scroll_end();
            let drawn = draw_at(&mut details, Rect { height, ..pane });
            assert_eq!(drawn.len(), usize::from(height));
            assert_eq!(check(&details, &drawn), in_view, "{height}");
        }
    }
}
