//! The Lineage section at the end of the thread details: the open thread's parent, its forks,
//! the agents it delegated to and the context transfers that name it. Enter or a click opens a
//! related thread the way the sidebar opens one.
//!
//! The rules are those of T3 nightly v0.0.46-nightly.20261007.2787 (commit f570bd2):
//!
//! - The relationships, their statuses and their order are `deriveThreadRelationshipGraph`,
//!   `immediateThreadRelationships`, `threadRelationshipRowStatus`, `orderWebThreadLineageRows`
//!   and `resolveMergeBackTargetThreadId` in
//!   packages/client-runtime/src/state/threadRelationships.ts, which the package exports as
//!   `@t3tools/client-runtime/state/thread-relationships`.
//! - The groups, the paging, the running count and each row's words are
//!   `ThreadRelationshipsPanel` in apps/web/src/components/chat/ThreadRelationshipsControl.tsx.
//! - The status words and their colors are `threadRelationshipStatusLabel` and
//!   `ThreadRelationshipIcon` in apps/web/src/components/chat/ThreadRelationshipIcon.tsx.
//! - A subagent's title is `formatSubagentDisplayTitle` in
//!   packages/client-runtime/src/state/subagentDisplay.ts, with the prompt standing in for a
//!   missing title as `projectedSubagentsToRuntime` in subagentRuntime.ts has it.
//!
//! The desktop reads the shells of every environment it knows and the archived threads it has
//! fetched. t3term has one server's live shell and fetches no archive. A related thread the
//! shell doesn't list, such as an archived one, shows only when the open thread or its
//! projection names it, as a parent or an agent, and it doesn't open. An archived fork doesn't
//! show. Where the desktop shows an agent's elapsed time, t3term shows its status word, and it
//! has no Stop, Merge back or Disconnect controls.
//!
//! Only the relationships that touch the open thread are worked out, from the shell and the
//! open thread's projection, once after an event that can change them, and never for a frame.
//! Only the rows its groups show get a title, so paging decides how many titles are cleaned.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};
use unicode_width::UnicodeWidthStr;

use super::setup::bounded;
use super::theme::{Theme, parse_iso_ms};
use super::{clip, row};

/// The rows each group shows at first, and how many more each Show more adds: the desktop's
/// `THREAD_LINEAGE_INITIAL_COUNT` and `THREAD_LINEAGE_PAGE_COUNT`.
pub(super) const FIRST_PAGE: usize = 6;
pub(super) const PAGE: usize = 12;
/// The most characters kept of a title.
const TITLE: usize = 256;
/// The longest thread id read. A longer one, or a blank one, links nothing.
const ID: usize = 512;
/// The longest subagent title read as a Codex task path.
const TASK_PATH: usize = 4096;
/// The longest timestamp read.
const TIMESTAMP: usize = 64;
/// The fewest columns a row's title keeps before its status word gives way.
const NARROWEST_TITLE: usize = 4;
/// The statuses that put a delegated agent under Previous agents.
const SETTLED: [&str; 6] = [
    "completed",
    "failed",
    "error",
    "cancelled",
    "interrupted",
    "idle",
];

/// The section's groups, in the order they show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Group {
    /// The parent, forks and context transfers, under no heading.
    Related,
    /// Delegated agents still at work, under no heading.
    Active,
    /// Delegated agents that settled, under a heading that starts closed.
    Previous,
}

impl Group {
    fn index(self) -> usize {
        match self {
            Group::Related => 0,
            Group::Active => 1,
            Group::Previous => 2,
        }
    }
}

/// How much of each group shows. It starts over for each thread, as the desktop's does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Paging {
    shown: [usize; 3],
    previous_open: bool,
}

impl Default for Paging {
    fn default() -> Paging {
        Paging {
            shown: [FIRST_PAGE; 3],
            previous_open: false,
        }
    }
}

impl Paging {
    pub(super) fn show_more(&mut self, group: Group) {
        let shown = &mut self.shown[group.index()];
        *shown = shown.saturating_add(PAGE);
    }

    pub(super) fn toggle_previous(&mut self) {
        self.previous_open = !self.previous_open;
    }

    pub(super) fn previous_open(&self) -> bool {
        self.previous_open
    }
}

/// What Enter or a click acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Target {
    /// A related thread's row, by the thread's id.
    Thread(String),
    /// The Show more row under a group.
    More(Group),
    /// The Previous agents heading, which opens and closes the group.
    Previous,
}

/// What a related thread is to the open one: `relationshipLabel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Relation {
    ParentThread,
    ParentAgent,
    Fork,
    Subagent,
    Transfer,
}

impl Relation {
    pub(super) fn name(self) -> &'static str {
        match self {
            Relation::ParentThread => "Parent thread",
            Relation::ParentAgent => "Parent agent",
            Relation::Fork => "Fork",
            Relation::Subagent => "Subagent",
            Relation::Transfer => "Context transfer",
        }
    }

    /// The mark at the start of the row, for the desktop's icon: an arrow up for a parent, a
    /// diamond for an agent and a turn down for a fork or a transfer.
    fn mark(self) -> &'static str {
        match self {
            Relation::ParentThread | Relation::ParentAgent => "↑",
            Relation::Subagent => "◆",
            Relation::Fork | Relation::Transfer => "↳",
        }
    }
}

/// The color of the dot on the desktop's icon, which the mark takes here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Live,
    Failed,
    Done,
    Quiet,
}

impl Tone {
    fn of(status: Option<&str>) -> Tone {
        match status {
            Some("running" | "in_progress" | "pending" | "waiting") => Tone::Live,
            Some("failed" | "error") => Tone::Failed,
            Some("completed") => Tone::Done,
            _ => Tone::Quiet,
        }
    }

    fn color(self, theme: &Theme) -> Color {
        match self {
            Tone::Live => theme.info,
            Tone::Failed => theme.error_fg,
            Tone::Done => theme.success,
            Tone::Quiet => theme.muted,
        }
    }
}

/// `threadRelationshipStatusLabel`.
fn status_word(status: Option<&str>) -> &'static str {
    match status.unwrap_or_default() {
        "preparing" | "starting" => "Starting",
        "running" | "in_progress" => "Running",
        "pending" | "queued" => "Queued",
        "waiting" | "blocked" => "Waiting",
        "completed" => "Done",
        "failed" | "error" => "Failed",
        "cancelled" | "interrupted" => "Stopped",
        "rolled_back" => "Reverted",
        "resolved_native" => "Resolved (native)",
        "resolved_portable" => "Resolved (portable)",
        "consumed" => "Consumed",
        "superseded" => "Superseded",
        "idle" => "Idle",
        _ => "Unknown",
    }
}

/// One related thread, as its row shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    pub id: String,
    title: String,
    pub relation: Relation,
    word: &'static str,
    tone: Tone,
    /// Whether the shell lists the thread, so it can open.
    pub available: bool,
}

impl Row {
    /// The row at `width`: the mark, the title cut to fit and the status word at the right,
    /// which gives way on a row too narrow for both.
    fn line(&self, width: usize, theme: &Theme) -> Line<'static> {
        let tone = self.tone.color(theme);
        let mark = Span::styled(self.relation.mark(), Style::new().fg(tone));
        if width < 2 {
            return Line::from(vec![mark]);
        }
        let word_width = self.word.width();
        let with_word = width >= 2 + NARROWEST_TITLE + 1 + word_width;
        let room = if with_word {
            width - 3 - word_width
        } else {
            width - 2
        };
        let title = if self.available {
            theme.fg
        } else {
            theme.muted
        };
        let left = vec![
            mark,
            Span::raw(" "),
            Span::styled(clip(&self.title, room), Style::new().fg(title)),
        ];
        let word = if self.tone == Tone::Failed {
            theme.error_fg
        } else {
            theme.muted
        };
        let right = if with_word {
            vec![Span::styled(self.word, Style::new().fg(word))]
        } else {
            Vec::new()
        };
        row(left, right, width)
    }
}

/// A group with the rows it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Part {
    group: Group,
    /// How many related threads the group has, shown or not.
    total: usize,
    /// How many of them failed, by the status the group goes by.
    failed: usize,
    /// Whether its rows show. Only Previous agents closes.
    open: bool,
    /// The rows in view: the group's first ones, as many as its paging shows.
    rows: Vec<Row>,
}

impl Part {
    /// How many more rows its Show more adds, or none when it has none.
    fn more(&self) -> usize {
        if self.open {
            (self.total - self.rows.len()).min(PAGE)
        } else {
            0
        }
    }
}

/// The section for one thread.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Lineage {
    /// The open thread these describe. None when no thread is open.
    thread_id: Option<String>,
    /// Delegated agents running, those without a thread yet included.
    running: usize,
    /// The groups that have rows, in order.
    parts: Vec<Part>,
    /// Every related thread, shown or not, to tell which events can change the section.
    related: HashSet<String>,
}

/// How one thread came from another, for the relationships that touch the open thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Fork,
    Subagent,
    Transfer,
}

#[derive(Debug, Clone, Copy)]
struct Edge<'a> {
    source: &'a str,
    target: &'a str,
    kind: Kind,
    status: Option<&'a str>,
}

/// The edges in the order they first came, as the desktop's map keeps them.
#[derive(Default)]
struct Edges<'a> {
    list: Vec<Edge<'a>>,
    at: HashMap<(&'a str, &'a str, Kind), usize>,
}

impl<'a> Edges<'a> {
    /// `addEdge`: a later edge between the same two threads of the same kind takes the
    /// earlier one's place. A thread is never its own relation, which the desktop checks only
    /// for transfers.
    fn add(&mut self, edge: Edge<'a>) {
        if edge.source == edge.target {
            return;
        }
        match self.at.entry((edge.source, edge.target, edge.kind)) {
            Entry::Occupied(at) => self.list[*at.get()] = edge,
            Entry::Vacant(slot) => {
                slot.insert(self.list.len());
                self.list.push(edge);
            }
        }
    }
}

/// A related thread before its row is made.
struct Candidate<'a> {
    id: &'a str,
    edge: Edge<'a>,
    rank: u8,
    created: Option<i64>,
}

/// An id T3 could have sent: a string that isn't blank or longer than `ID`.
fn id_of(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|id| id.len() <= ID && !id.trim().is_empty())
}

/// The thread `thread` came from: the thread of the run it was forked from, or else its
/// lineage's parent.
pub(super) fn parent_of(thread: &Value) -> Option<&str> {
    let forked = &thread["forkedFrom"];
    if forked["type"] == "run" {
        id_of(forked.get("threadId"))
    } else {
        id_of(thread["lineage"].get("parentThreadId"))
    }
}

/// A thread's live run status, or else its status.
fn run_status(thread: &Value) -> Option<&str> {
    thread
        .get("activityRunStatus")
        .and_then(Value::as_str)
        .or_else(|| thread.get("status").and_then(Value::as_str))
}

fn list<'a>(projection: Option<&'a Map<String, Value>>, key: &str) -> &'a [Value] {
    projection
        .and_then(|projection| projection.get(key))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Newest first, and a thread without a time after every thread with one.
fn newest_first(a: Option<i64>, b: Option<i64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

impl Lineage {
    /// The section for thread `open_id`, from the shell's `threads` and the thread's
    /// `projection`, which counts only while it is the open thread's. `paging` says how many
    /// rows of each group get made.
    pub(super) fn of(
        open_id: Option<&str>,
        threads: &[Value],
        projection: Option<&Map<String, Value>>,
        paging: &Paging,
    ) -> Lineage {
        let Some(current) = open_id else {
            return Lineage::default();
        };
        // The first copy of each thread, which the desktop takes as the one to trust.
        let mut by_id: HashMap<&str, &Value> = HashMap::with_capacity(threads.len());
        for thread in threads {
            if let Some(id) = id_of(thread.get("id")) {
                by_id.entry(id).or_insert(thread);
            }
        }

        let mut edges = Edges::default();
        for thread in threads {
            let Some(id) = id_of(thread.get("id")) else {
                continue;
            };
            // A later copy of a thread counts for nothing.
            if !std::ptr::eq(by_id[id], thread) {
                continue;
            }
            let Some(parent) = parent_of(thread) else {
                continue;
            };
            if parent != current && id != current {
                continue;
            }
            let kind = if thread["lineage"]["relationshipToParent"] == "subagent" {
                Kind::Subagent
            } else {
                Kind::Fork
            };
            edges.add(Edge {
                source: parent,
                target: id,
                kind,
                status: run_status(thread),
            });
        }

        let projection = projection.filter(|projection| {
            let thread = projection.get("thread");
            id_of(thread.and_then(|thread| thread.get("id"))) == Some(current)
        });
        let subagents = list(projection, "subagents");
        // `subagentsByThreadId`: the last record for each child thread.
        let mut agents: HashMap<&str, &Value> = HashMap::new();
        for subagent in subagents {
            let Some(child) = id_of(subagent.get("childThreadId")) else {
                continue;
            };
            agents.insert(child, subagent);
            // A delegated task settles with its first run, but a live run on its thread
            // outranks that.
            let live = by_id
                .get(child)
                .copied()
                .and_then(|thread| thread.get("activityRunStatus"))
                .and_then(Value::as_str);
            let settled = subagent.get("status").and_then(Value::as_str);
            edges.add(Edge {
                source: current,
                target: child,
                kind: Kind::Subagent,
                status: live.or(settled),
            });
        }
        for transfer in list(projection, "contextTransfers") {
            let source = id_of(transfer.get("sourceThreadId"));
            let target = id_of(transfer.get("targetThreadId"));
            let (Some(source), Some(target)) = (source, target) else {
                continue;
            };
            if source != current && target != current {
                continue;
            }
            edges.add(Edge {
                source,
                target,
                kind: Kind::Transfer,
                status: transfer.get("status").and_then(Value::as_str),
            });
        }

        let merge_target = projection
            .and_then(|projection| projection.get("thread"))
            .filter(|thread| thread["lineage"]["relationshipToParent"] == "fork")
            .and_then(parent_of);
        let is_parent = |edge: &Edge| edge.kind != Kind::Transfer && edge.target == current;
        // `immediateThreadRelationships`: the first edge to each related thread.
        let mut seen = HashSet::new();
        let mut related = Vec::new();
        for edge in &edges.list {
            let id = if edge.source == current {
                edge.target
            } else {
                edge.source
            };
            if !seen.insert(id) {
                continue;
            }
            let rank = if is_parent(edge) {
                0
            } else if Some(id) == merge_target {
                1
            } else {
                2
            };
            let created = by_id
                .get(id)
                .and_then(|thread| thread.get("createdAt"))
                .and_then(Value::as_str)
                .filter(|time| time.len() <= TIMESTAMP)
                .and_then(parse_iso_ms);
            related.push(Candidate {
                id,
                edge: *edge,
                rank,
                created,
            });
        }
        // `orderWebThreadLineageRows`: the parent, then the thread a fork merges back to, then
        // the newest first, then by id.
        related.sort_by(|a, b| {
            a.rank
                .cmp(&b.rank)
                .then_with(|| newest_first(a.created, b.created))
                .then_with(|| a.id.cmp(b.id))
        });

        let group = |candidate: &Candidate| {
            let edge = &candidate.edge;
            if edge.kind != Kind::Subagent || is_parent(edge) {
                Group::Related
            } else if edge.status.is_some_and(|status| SETTLED.contains(&status)) {
                Group::Previous
            } else {
                Group::Active
            }
        };
        // Agents without a thread yet have no row, so they count on their own.
        let unseen = subagents
            .iter()
            .filter(|agent| agent.get("childThreadId") == Some(&Value::Null))
            .filter(|agent| agent["status"] == "running")
            .count();
        let working = related
            .iter()
            .filter(|c| group(c) == Group::Active && c.edge.status == Some("running"))
            .count();
        let running = unseen + working;

        let mut parts = Vec::new();
        for kind in [Group::Related, Group::Active, Group::Previous] {
            let members: Vec<_> = related.iter().filter(|c| group(c) == kind).collect();
            if members.is_empty() {
                continue;
            }
            let open = kind != Group::Previous || paging.previous_open;
            let shown = if open {
                paging.shown[kind.index()].min(members.len())
            } else {
                0
            };
            let failed = members
                .iter()
                .filter(|c| matches!(c.edge.status, Some("failed" | "error")))
                .count();
            let rows = members[..shown]
                .iter()
                .map(|candidate| row_for(candidate, current, &by_id, &agents))
                .collect();
            parts.push(Part {
                group: kind,
                total: members.len(),
                failed,
                open,
                rows,
            });
        }

        Lineage {
            thread_id: Some(current.to_string()),
            running,
            parts,
            related: seen.into_iter().map(str::to_string).collect(),
        }
    }

    pub(super) fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    /// Whether thread `id` is related to the open one, so an event for it can change the
    /// section.
    pub(super) fn relates(&self, id: &str) -> bool {
        self.related.contains(id)
    }

    /// Whether the section shows: it has a related thread, or a delegated agent runs.
    pub(super) fn shows(&self) -> bool {
        !self.parts.is_empty() || self.running > 0
    }

    /// Whether it has anything Enter or a click acts on.
    pub(super) fn has_targets(&self) -> bool {
        !self.parts.is_empty()
    }

    /// What Enter or a click acts on, in the order the rows show.
    pub(super) fn targets(&self) -> Vec<Target> {
        let mut targets = Vec::new();
        for part in &self.parts {
            if part.group == Group::Previous {
                targets.push(Target::Previous);
            }
            targets.extend(part.rows.iter().map(|row| Target::Thread(row.id.clone())));
            if part.more() > 0 {
                targets.push(Target::More(part.group));
            }
        }
        targets
    }

    pub(super) fn has(&self, target: &Target) -> bool {
        match target {
            Target::Thread(id) => self.row(id).is_some(),
            Target::More(group) => self.more(*group) > 0,
            Target::Previous => self.parts.iter().any(|part| part.group == Group::Previous),
        }
    }

    /// The row in view for thread `id`.
    pub(super) fn row(&self, id: &str) -> Option<&Row> {
        self.parts
            .iter()
            .flat_map(|part| &part.rows)
            .find(|row| row.id == id)
    }

    /// The last row in view of `group`.
    pub(super) fn last_row(&self, group: Group) -> Option<&Row> {
        self.parts
            .iter()
            .find(|part| part.group == group)
            .and_then(|part| part.rows.last())
    }

    /// How many rows Show more adds to `group`.
    pub(super) fn more(&self, group: Group) -> usize {
        self.parts
            .iter()
            .find(|part| part.group == group)
            .map_or(0, Part::more)
    }

    /// Appends the section's rows at `width` to `lines`, and to `targets` the index of each row
    /// Enter or a click acts on, in the order of `targets`. The heading counts running agents.
    /// Previous agents counts its rows while closed and its failed ones always, with a rule
    /// between, as the desktop's collapsible heading does.
    pub(super) fn lines(
        &self,
        width: usize,
        theme: &Theme,
        lines: &mut Vec<Line<'static>>,
        targets: &mut Vec<(usize, Target)>,
    ) {
        if width == 0 || !self.shows() {
            return;
        }
        let muted = Style::new().fg(theme.muted);
        let heading = if self.running > 0 {
            format!("Lineage · {} running", self.running)
        } else {
            "Lineage".to_string()
        };
        lines.push(Line::styled(clip(&heading, width), muted));
        for part in &self.parts {
            if part.group == Group::Previous {
                targets.push((lines.len(), Target::Previous));
                lines.push(previous_heading(part, width, theme));
            }
            for row in &part.rows {
                targets.push((lines.len(), Target::Thread(row.id.clone())));
                lines.push(row.line(width, theme));
            }
            let more = part.more();
            if more > 0 {
                targets.push((lines.len(), Target::More(part.group)));
                let text = clip(&format!("+ Show {more} more"), width);
                lines.push(row(vec![Span::styled(text, muted)], Vec::new(), width));
            }
        }
    }
}

/// The Previous agents heading: a chevron, the label with the group's count while closed, a
/// rule, and the failed count in the error color. On a narrow row the count of failed agents
/// keeps its place and the label is cut.
fn previous_heading(part: &Part, width: usize, theme: &Theme) -> Line<'static> {
    let label = if part.open {
        "⌄ Previous agents".to_string()
    } else {
        format!("› Previous agents ({})", part.total)
    };
    let failed = if part.failed > 0 {
        format!(" {} failed", part.failed)
    } else {
        String::new()
    };
    let failed_width = failed.width();
    let room = if failed_width > 0 && width >= failed_width + 8 {
        width - failed_width
    } else {
        width
    };
    let label = clip(&label, room);
    let left = width - label.width();
    let mut used = label.width();
    let mut spans = vec![Span::styled(label, Style::new().fg(theme.muted))];
    if left >= failed_width + 2 {
        let rule = format!(" {}", "─".repeat(left - failed_width - 1));
        used += rule.width();
        spans.push(Span::styled(rule, Style::new().fg(theme.border)));
    }
    if failed_width > 0 && width - used >= failed_width {
        used += failed_width;
        spans.push(Span::styled(failed, Style::new().fg(theme.error_fg)));
    }
    spans.push(Span::raw(" ".repeat(width - used)));
    Line::from(spans)
}

/// The row for a related thread, with the words and title the desktop gives it.
fn row_for(
    candidate: &Candidate,
    current: &str,
    by_id: &HashMap<&str, &Value>,
    agents: &HashMap<&str, &Value>,
) -> Row {
    let Candidate { id, edge, .. } = *candidate;
    let parent = edge.kind != Kind::Transfer && edge.target == current;
    let thread = by_id.get(id).copied();
    // `threadRelationshipRowStatus`: a parent shows its own activity, every other row its
    // edge's status.
    let status = if edge.kind == Kind::Transfer || id == edge.target {
        edge.status
    } else {
        thread.and_then(run_status)
    };
    let relation = match (edge.kind, parent) {
        (Kind::Transfer, _) => Relation::Transfer,
        (Kind::Subagent, true) => Relation::ParentAgent,
        (Kind::Subagent, false) => Relation::Subagent,
        (Kind::Fork, true) => Relation::ParentThread,
        (Kind::Fork, false) => Relation::Fork,
    };
    let subagent = edge.kind == Kind::Subagent;
    let agent = if subagent && !parent {
        agents.get(id).copied()
    } else {
        None
    };
    // The shell's title, the agent's, then the id. A title that cleans to nothing gives way
    // to the next, where the desktop would show it blank.
    let raw = thread
        .and_then(|thread| thread.get("title"))
        .and_then(Value::as_str)
        .map(Cow::Borrowed)
        .or_else(|| agent.and_then(agent_title));
    let mut title = raw.map(|raw| display(&raw, subagent)).unwrap_or_default();
    if title.is_empty() {
        title = display(id, subagent);
    }
    if title.is_empty() {
        title = "Untitled".to_string();
    }
    // The active shell drops a thread T3 archives, so a copy that says it is archived is
    // stale, and doesn't open either.
    let available = thread.is_some_and(|thread| thread["archivedAt"].is_null());
    let mut word = status_word(status);
    if !available && word == "Unknown" {
        word = "Unavailable";
    }
    Row {
        id: id.to_string(),
        title,
        relation,
        word,
        tone: Tone::of(status),
        available,
    }
}

/// `projectedSubagentsToRuntime`'s title: the agent's own, or else its prompt, cut to its
/// first 77 characters and `...` when it has more than 80.
fn agent_title(subagent: &Value) -> Option<Cow<'_, str>> {
    if let Some(title) = subagent.get("title").and_then(Value::as_str) {
        return Some(Cow::Borrowed(title));
    }
    let prompt = subagent.get("prompt").and_then(Value::as_str)?;
    if prompt.char_indices().nth(80).is_none() {
        return Some(Cow::Borrowed(prompt));
    }
    let end = prompt
        .char_indices()
        .nth(77)
        .map_or(prompt.len(), |(at, _)| at);
    Some(Cow::Owned(format!("{}...", &prompt[..end])))
}

/// A title as its row shows it, cleaned and bounded. An agent's loses a `Subagent:` prefix,
/// and a Codex task path reads as its last part in words (`formatSubagentDisplayTitle`).
fn display(raw: &str, subagent: bool) -> String {
    if !subagent {
        return bounded(raw, TITLE);
    }
    let text = without_prefix(raw);
    match task_name(text) {
        Some(name) => bounded(&name, TITLE),
        None => bounded(text, TITLE),
    }
}

/// `title` without a `Subagent:` prefix in any case of its letters, and the white space after
/// it, of which it reads at most `TITLE * 4` characters.
fn without_prefix(title: &str) -> &str {
    if !title
        .get(..9)
        .is_some_and(|head| head.eq_ignore_ascii_case("subagent:"))
    {
        return title;
    }
    let rest = &title[9..];
    let mut start = rest.len();
    for (index, (at, c)) in rest.char_indices().enumerate() {
        if !c.is_whitespace() || index == TITLE * 4 {
            start = at;
            break;
        }
    }
    &rest[start..]
}

/// The last part of a Codex task path such as `/root/review/fix_the_parser`, in words that
/// start with capitals: `Fix The Parser`. None for anything else, or a path longer than
/// `TASK_PATH` bytes.
fn task_name(text: &str) -> Option<String> {
    if text.len() > TASK_PATH {
        return None;
    }
    let rest = text.strip_prefix("/root/")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.split('/').any(str::is_empty) {
        return None;
    }
    let last = rest.rsplit('/').next()?;
    let words: Vec<String> = last
        .split(|c: char| c == '_' || c.is_whitespace())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Depth;
    use serde_json::json;

    /// A thread as the shell lists it, created on day `day` of October 2026.
    fn shell(id: &str, title: &str, status: &str, day: u32) -> Value {
        json!({"id": id, "title": title, "status": status, "activityRunStatus": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
            "forkedFrom": null, "createdAt": format!("2026-10-{day:02}T00:00:00.000Z")})
    }

    /// `thread` as a fork or a subagent of thread `parent`, by `relationship`.
    fn under(mut thread: Value, parent: &str, relationship: &str) -> Value {
        thread["lineage"]["parentThreadId"] = json!(parent);
        thread["lineage"]["relationshipToParent"] = json!(relationship);
        thread
    }

    fn agent(id: &str, child: Value, status: &str, title: Value) -> Value {
        json!({"id": id, "threadId": "t", "childThreadId": child, "status": status,
            "title": title, "prompt": "Look into it"})
    }

    fn transfer(source: &str, target: &str, status: &str) -> Value {
        json!({"type": "provider_handoff", "sourceThreadId": source, "targetThreadId": target,
            "status": status})
    }

    fn projected(thread: Value, agents: Vec<Value>, transfers: Vec<Value>) -> Map<String, Value> {
        let value = json!({"thread": thread, "subagents": agents, "contextTransfers": transfers});
        value.as_object().cloned().unwrap()
    }

    fn rule(columns: usize) -> String {
        "─".repeat(columns)
    }

    /// The section's rows at `width`, each with its runs of spaces made one.
    fn shown(lineage: &Lineage, width: usize) -> Vec<String> {
        let theme = Theme::new(Depth::TrueColor);
        let mut lines = Vec::new();
        let mut targets = Vec::new();
        lineage.lines(width, &theme, &mut lines, &mut targets);
        if width > 0 {
            let order: Vec<Target> = targets.into_iter().map(|(_, target)| target).collect();
            assert_eq!(order, lineage.targets());
        }
        lines
            .iter()
            .map(|line| {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(text.width() <= width, "{text:?} at {width}");
                text.split_whitespace().collect::<Vec<_>>().join(" ")
            })
            .collect()
    }

    /// Thread `t`, a fork of `p`, with two forks, two agents and two agents without a thread.
    fn family() -> (Vec<Value>, Map<String, Value>) {
        let mut current = under(shell("t", "Fix the parser", "running", 2), "p", "fork");
        current["forkedFrom"] = json!({"type": "run", "threadId": "p", "runId": "r"});
        let task = shell("a2", "Subagent: /root/ci/run_the_tests", "completed", 6);
        let mut live = under(task, "t", "subagent");
        live["activityRunStatus"] = json!("running");
        let threads = vec![
            shell("p", "Plan the release", "idle", 1),
            current.clone(),
            under(shell("f1", "Try a lexer", "completed", 3), "t", "fork"),
            under(shell("f2", "Try a table", "idle", 5), "t", "fork"),
            under(shell("a1", "Read docs", "completed", 4), "t", "subagent"),
            live,
            under(shell("x", "Unrelated", "running", 7), "p", "fork"),
        ];
        let projection = projected(
            current,
            vec![
                agent("s1", json!("a1"), "completed", Value::Null),
                // The task settled with its first run, but its thread runs again.
                agent("s2", json!("a2"), "completed", Value::Null),
                agent("s3", Value::Null, "running", json!("Not started")),
                agent("s4", Value::Null, "pending", Value::Null),
            ],
            Vec::new(),
        );
        (threads, projection)
    }

    #[test]
    fn the_parent_forks_and_agents_come_in_the_desktops_order() {
        let (threads, projection) = family();
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &Paging::default());
        let closed = format!("› Previous agents (1) {}", rule(18));
        assert_eq!(
            shown(&lineage, 40),
            [
                "Lineage · 2 running",
                "↑ Plan the release Idle",
                "↳ Try a table Idle",
                "↳ Try a lexer Done",
                "◆ Run The Tests Running",
                closed.as_str(),
            ]
        );
        let row = lineage.row("p").unwrap();
        assert_eq!(row.relation, Relation::ParentThread);
        assert!(row.available);
        assert_eq!(lineage.row("a2").unwrap().relation, Relation::Subagent);
        // The other fork of p is no relation of t.
        assert!(!lineage.relates("x"));
        assert!(lineage.relates("a1") && lineage.relates("p"));

        // Previous agents opens to its rows.
        let mut paging = Paging::default();
        paging.toggle_previous();
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &paging);
        let rows = shown(&lineage, 40);
        let open = format!("⌄ Previous agents {}", rule(22));
        assert_eq!(rows[5..], [open.as_str(), "◆ Read docs Done"]);
        assert_eq!(lineage.row("a1").unwrap().relation, Relation::Subagent);
    }

    #[test]
    fn a_live_run_outranks_the_settled_task_until_it_ends() {
        let (mut threads, projection) = family();
        let paging = Paging::default();
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &paging);
        assert_eq!(lineage.row("a2").map(|row| row.word), Some("Running"));
        // The run ends, and the task's own status stands again.
        threads[5]["activityRunStatus"] = Value::Null;
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &paging);
        assert!(lineage.row("a2").is_none(), "Previous agents is closed");
        let rows = shown(&lineage, 40);
        assert_eq!(rows[0], "Lineage · 1 running");
        let closed = "› Previous agents (2) ";
        assert!(rows.iter().any(|row| row.starts_with(closed)));
        // A run that waits or starts is live too, under its own word.
        for (live, word) in [("waiting", "Waiting"), ("preparing", "Starting")] {
            threads[5]["activityRunStatus"] = json!(live);
            let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &paging);
            assert_eq!(lineage.row("a2").map(|row| row.word), Some(word));
        }
    }

    #[test]
    fn a_child_thread_shows_its_parent_agent() {
        let (threads, _) = family();
        let lineage = Lineage::of(Some("a2"), &threads, None, &Paging::default());
        let rows = shown(&lineage, 40);
        assert_eq!(rows, ["Lineage", "↑ Fix the parser Running"]);
        assert_eq!(lineage.row("t").unwrap().relation, Relation::ParentAgent);
        assert_eq!(lineage.targets(), [Target::Thread("t".into())]);
    }

    #[test]
    fn a_hundred_and_twenty_four_previous_agents_page_by_twelve() {
        let mut threads = vec![shell("t", "Lead", "idle", 1)];
        let mut agents = Vec::new();
        for i in 0..124 {
            let id = format!("a{i:03}");
            let status = if i % 18 == 0 { "failed" } else { "completed" };
            let child = shell(&id, &format!("Agent {i}"), status, 2);
            threads.push(under(child, "t", "subagent"));
            agents.push(agent(&format!("s{i}"), json!(id), status, Value::Null));
        }
        let projection = projected(threads[0].clone(), agents, Vec::new());
        let mut paging = Paging::default();
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &paging);
        let closed = format!("› Previous agents (124) {} 7 failed", rule(7));
        assert_eq!(shown(&lineage, 40), ["Lineage", closed.as_str()]);
        assert_eq!(lineage.targets(), [Target::Previous]);
        // At the pane's width the rule gives way and the failed count stays.
        assert_eq!(shown(&lineage, 32)[1], "› Previous agents (124) 7 failed");
        // Narrower, the label is cut before the count.
        assert_eq!(shown(&lineage, 20)[1], "› Previous… 7 failed");

        paging.toggle_previous();
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &paging);
        let rows = shown(&lineage, 40);
        let open = format!("⌄ Previous agents {} 7 failed", rule(13));
        assert_eq!(rows[1], open);
        assert_eq!(rows.len(), 2 + FIRST_PAGE + 1);
        // All were made at once, so they go by id.
        assert_eq!(rows[2], "◆ Agent 0 Failed");
        assert_eq!(rows[3], "◆ Agent 1 Done");
        assert_eq!(rows[rows.len() - 1], "+ Show 12 more");
        let mut total = FIRST_PAGE;
        while total < 124 {
            paging.show_more(Group::Previous);
            total = (total + PAGE).min(124);
            let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &paging);
            assert_eq!(lineage.parts[0].rows.len(), total);
            assert_eq!(lineage.more(Group::Previous) > 0, total < 124);
            if total == 114 {
                assert_eq!(shown(&lineage, 40).last().unwrap(), "+ Show 10 more");
            }
        }
    }

    #[test]
    fn each_group_pages_on_its_own() {
        let mut threads = vec![shell("t", "Lead", "idle", 1)];
        for i in 0..8 {
            let fork = shell(&format!("f{i}"), "Fork", "idle", 2);
            threads.push(under(fork, "t", "fork"));
        }
        let mut paging = Paging::default();
        let lineage = Lineage::of(Some("t"), &threads, None, &paging);
        assert_eq!(lineage.more(Group::Related), 2);
        assert_eq!(shown(&lineage, 40).last().unwrap(), "+ Show 2 more");
        paging.show_more(Group::Related);
        let lineage = Lineage::of(Some("t"), &threads, None, &paging);
        assert_eq!(lineage.more(Group::Related), 0);
        assert_eq!(lineage.targets().len(), 8);
    }

    #[test]
    fn duplicates_self_links_cycles_and_bad_ids_link_nothing_twice() {
        let mut threads = vec![
            under(shell("t", "Lead", "idle", 1), "u", "fork"),
            // u is t's parent and its fork, which makes one row, the first edge's.
            under(shell("u", "Loop", "idle", 2), "t", "fork"),
            // A thread that is its own parent.
            under(shell("s", "Self", "idle", 3), "s", "fork"),
            under(shell("c", "Child", "completed", 4), "t", "subagent"),
            // A second copy of c, with another parent, counts for nothing.
            under(shell("c", "Copy", "completed", 4), "z", "subagent"),
        ];
        let bad = [
            json!(""),
            json!("   "),
            json!(7),
            json!("x".repeat(ID + 1)),
            Value::Null,
        ];
        for (n, bad) in bad.into_iter().enumerate() {
            let mut thread = under(shell("bad", "Bad", "idle", 5), "t", "fork");
            thread["id"] = bad.clone();
            threads.push(thread);
            let mut orphan = shell(&format!("orphan{n}"), "Orphan", "idle", 5);
            orphan["lineage"]["parentThreadId"] = bad;
            threads.push(orphan);
        }
        let to_itself = transfer("t", "t", "pending");
        let projection = projected(
            threads[0].clone(),
            vec![
                agent("s1", json!("c"), "completed", Value::Null),
                agent("s2", json!("t"), "running", Value::Null),
                agent("s3", json!(""), "running", Value::Null),
                agent("s4", json!(["c"]), "failed", Value::Null),
            ],
            vec![to_itself],
        );
        let mut paging = Paging::default();
        paging.toggle_previous();
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &paging);
        let u = Target::Thread("u".into());
        let c = Target::Thread("c".into());
        assert_eq!(lineage.targets(), [u, Target::Previous, c]);
        assert_eq!(lineage.row("c").unwrap().title, "Child");
        assert_eq!(lineage.row("u").unwrap().relation, Relation::ParentThread);
        // No agent without a thread runs, and a malformed one doesn't count.
        assert_eq!(lineage.running, 0);
        assert!(!Lineage::of(Some("s"), &threads, None, &paging).shows());
    }

    #[test]
    fn missing_and_archived_threads_show_without_opening() {
        let mut archived = under(shell("old", "Old try", "idle", 2), "t", "fork");
        archived["archivedAt"] = json!("2026-10-05T00:00:00.000Z");
        let lead = under(shell("t", "Lead", "idle", 1), "gone", "fork");
        let threads = vec![lead, archived];
        let lost = agent("s1", json!("lost"), "completed", json!("Lost agent"));
        let projection = projected(threads[0].clone(), vec![lost], Vec::new());
        let mut paging = Paging::default();
        paging.toggle_previous();
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &paging);
        let open = format!("⌄ Previous agents {}", rule(22));
        assert_eq!(
            shown(&lineage, 40),
            [
                "Lineage",
                "↑ gone Unavailable",
                "↳ Old try Idle",
                open.as_str(),
                "◆ Lost agent Done",
            ]
        );
        for id in ["gone", "old", "lost"] {
            assert!(!lineage.row(id).unwrap().available, "{id}");
        }
    }

    #[test]
    fn transfers_that_touch_the_thread_show() {
        let threads = vec![
            shell("t", "Lead", "idle", 1),
            shell("y", "Handed from", "idle", 2),
            shell("z", "Handed to", "idle", 3),
        ];
        let transfers = vec![
            transfer("t", "z", "resolved_native"),
            transfer("y", "t", "consumed"),
            transfer("y", "z", "failed"),
            transfer("t", "t", "pending"),
        ];
        let projection = projected(threads[0].clone(), Vec::new(), transfers);
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &Paging::default());
        assert_eq!(
            shown(&lineage, 40),
            [
                "Lineage",
                "↳ Handed to Resolved (native)",
                "↳ Handed from Consumed",
            ]
        );
        assert_eq!(lineage.row("y").unwrap().relation, Relation::Transfer);
    }

    #[test]
    fn another_threads_projection_counts_for_nothing() {
        let (threads, mut projection) = family();
        projection.insert("thread".into(), shell("other", "Other", "idle", 1));
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &Paging::default());
        // The shell's agents still show, but no agent without a thread counts.
        assert_eq!(shown(&lineage, 40)[0], "Lineage · 1 running");
        assert!(lineage.row("a2").is_some());
    }

    #[test]
    fn titles_follow_the_desktop() {
        let task = "Subagent: /root/review/fix_the_parser";
        assert_eq!(display(task, true), "Fix The Parser");
        assert_eq!(display("subagent:\t/root/a/", true), "A");
        assert_eq!(display("/root/one/two words__x/", true), "Two Words X");
        assert_eq!(display("/root//x", true), "/root//x");
        assert_eq!(display("/root/", true), "/root/");
        assert_eq!(display("/root/___", true), "/root/___");
        assert_eq!(display("Subagent: plain", false), "Subagent: plain");
        let long = json!({"title": null, "prompt": "p".repeat(100)});
        let cut = format!("{}...", "p".repeat(77));
        assert_eq!(agent_title(&long).unwrap(), cut);
        let short = json!({"title": null, "prompt": "q".repeat(80)});
        assert_eq!(agent_title(&short).unwrap(), "q".repeat(80));
    }

    #[test]
    fn hostile_titles_stay_bounded_at_every_width() {
        let joined = "名e\u{301}👩\u{200d}💻".repeat(500_000);
        let hostile = format!("\u{1b}]52;c;eA==\u{7}{joined}\r\nevil");
        let mut task = under(shell("c", &hostile, "running", 2), "t", "subagent");
        task["activityRunStatus"] = json!("running");
        let mut blank = under(shell("\u{1b}[2Jb", "\u{7}", "idle", 3), "t", "fork");
        blank["createdAt"] = json!("9".repeat(1_000_000));
        let threads = vec![shell("t", "Lead", "idle", 1), task, blank];
        let prompt = "\u{9b}".repeat(2_000_000);
        let quiet = json!({"id": "s", "childThreadId": "d", "status": "running", "prompt": prompt});
        let projection = projected(threads[0].clone(), vec![quiet], Vec::new());
        let lineage = Lineage::of(Some("t"), &threads, Some(&projection), &Paging::default());
        let title = &lineage.row("c").unwrap().title;
        assert!(title.ends_with('…'));
        assert!(title.chars().count() <= TITLE + 1);
        assert!(!title.chars().any(char::is_control));
        // A title that cleans to nothing gives way to the id, cleaned.
        assert_eq!(lineage.row("\u{1b}[2Jb").unwrap().title, "[2Jb");
        // A prompt of nothing but controls leaves its cut mark.
        assert_eq!(lineage.row("d").unwrap().title, "...");
        for width in 0..=40 {
            for row in shown(&lineage, width) {
                assert!(!row.chars().any(char::is_control), "{row:?}");
            }
        }
        let rows = shown(&lineage, 40);
        assert!(rows.iter().any(|row| row.contains('名')));
    }

    #[test]
    fn every_status_has_the_desktops_word() {
        for (status, word) in [
            ("starting", "Starting"),
            ("in_progress", "Running"),
            ("queued", "Queued"),
            ("pending", "Queued"),
            ("blocked", "Waiting"),
            ("completed", "Done"),
            ("error", "Failed"),
            ("interrupted", "Stopped"),
            ("cancelled", "Stopped"),
            ("rolled_back", "Reverted"),
            ("resolved_portable", "Resolved (portable)"),
            ("superseded", "Superseded"),
            ("idle", "Idle"),
            ("\u{1b}[31m", "Unknown"),
        ] {
            assert_eq!(status_word(Some(status)), word);
        }
        assert_eq!(status_word(None), "Unknown");
    }
}
