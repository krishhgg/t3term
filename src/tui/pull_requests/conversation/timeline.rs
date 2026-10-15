//! A pull request's Timeline in a link's details, as the desktop's Timeline tab shows it
//! (apps/web/src/components/pullRequest/PullRequestTimelineTab.tsx, with
//! `buildPullRequestTimeline` and `groupPullRequestTimelineConversations` in
//! pullRequestDetail.logic.ts, at f570bd2).
//!
//! The pull request's opening, its commits, its remarks and its merge or close make one list,
//! newest first until the reader switches it. Each commit, each verdict with its text and each
//! change of state is a row of its own. A run of other remarks between two of those is one row,
//! closed at first, that opens to them. The header counts the remarks, the commits and the
//! approvals that still stand.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::super::{Answer, NAME, TITLE, flowed, grouped, joined, whole};
use super::{
    Control, Conversation, Outcome, Place, Remark, Rows, Said, Section, Shown, ago, instant,
};
use crate::pull_request;
use crate::tui::setup::bounded;
use crate::tui::theme::Theme;
use crate::tui::{clip, row};

/// The columns an entry's mark and the space after it take, where the details have more.
const GUTTER: usize = 2;

/// When the pull request opened, and merged or closed, and who opened it, from its detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui::pull_requests) struct Lifecycle {
    opened: Option<i64>,
    author: Option<String>,
    /// How it ended, and when. Merged wins over closed, as GitHub sets both on a merge.
    ended: Option<(Ended, Option<i64>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    Merged,
    Closed,
}

impl Lifecycle {
    pub(in crate::tui::pull_requests) fn of(detail: &pull_request::Detail) -> Lifecycle {
        // A time the host left out or sent empty is no event, as the desktop's test of it
        // reads. One that doesn't parse is an event at no time.
        let at = |time: &Option<String>| {
            let time = time.as_deref().filter(|time| !time.is_empty());
            time.map(instant)
        };
        let ended = match (at(&detail.merged_at), at(&detail.closed_at)) {
            (Some(merged), _) => Some((Ended::Merged, merged)),
            (None, Some(closed)) => Some((Ended::Closed, closed)),
            (None, None) => None,
        };
        Lifecycle {
            opened: instant(&detail.created_at),
            author: detail
                .author
                .as_ref()
                .map(|actor| bounded(&actor.login, NAME)),
            ended,
        }
    }
}

/// A commit on the branch, cleaned and bounded once, when the conversation arrives.
pub(in super::super) struct Commit {
    /// Its id as T3 sent it, which a read of its own diff names.
    pub(in super::super) oid: String,
    /// The first seven characters of its id, as the desktop shows them.
    pub(in super::super) short: String,
    /// Its headline, which may be empty.
    headline: String,
    pub(super) at: Option<i64>,
    /// The lines it adds and removes, where the host counted both.
    counts: Option<(u64, u64)>,
    /// Its first author's login, which stands in for the desktop's avatar.
    author: Option<String>,
}

impl Commit {
    pub(super) fn of(commit: &pull_request::Commit) -> Commit {
        let author = commit.authors.as_deref().and_then(<[_]>::first);
        Commit {
            oid: commit.oid.clone(),
            short: bounded(&commit.oid, NAME).chars().take(7).collect(),
            headline: bounded(&commit.headline, TITLE),
            at: instant(&commit.committed_at),
            counts: commit.additions.zip(commit.deletions),
            author: author.map(|actor| bounded(&actor.login, NAME)),
        }
    }

    /// Its headline, or `Untitled commit` where it has none, as the desktop's `CommitEvent`
    /// says.
    pub(in super::super) fn headline(&self) -> &str {
        match self.headline.is_empty() {
            true => "Untitled commit",
            false => &self.headline,
        }
    }
}

/// What an event on the Timeline is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum What {
    Opened,
    /// A commit, by its place among them as T3 sent them.
    Commit(usize),
    /// A remark that gives a verdict, which is a row of its own.
    Verdict(Place),
    /// Any other remark, which joins a run.
    Remark(Place),
    Merged,
    Closed,
}

struct Event {
    /// When it happened. None where the host's time doesn't parse.
    at: Option<i64>,
    what: What,
}

/// A row of the Timeline: one event, or a run of remarks between two others.
enum Row {
    Event(usize),
    /// The run's place among the runs newest first, and its events.
    Run(usize, Range<usize>),
}

/// The Timeline of a pull request, newest first.
pub(super) struct Timeline {
    events: Vec<Event>,
    rows: Vec<Row>,
    /// Each run's newest remark's id, which the run is known by while the reader opens it.
    keys: Vec<String>,
    /// Who opened the pull request: the conversation's author, else the detail's.
    opener: Option<String>,
    /// When the newest commit came. A verdict given before it is on earlier changes.
    newest: Option<i64>,
    /// The opening and ending it was laid out with.
    lifecycle: Lifecycle,
}

impl Timeline {
    /// The Timeline of `conversation`, with the pull request's opening and its merge or close
    /// from `lifecycle`, as `buildPullRequestTimeline` and
    /// `groupPullRequestTimelineConversations` make it. It sorts the events newest first by
    /// their instants, where the desktop compares the host's strings, and a time that doesn't
    /// parse sorts as the oldest. Events at the same time keep the desktop's order: the
    /// opening, the commits, the remarks, then the merge or close.
    pub(super) fn of(lifecycle: &Lifecycle, conversation: &Conversation) -> Timeline {
        let opened = Event {
            at: lifecycle.opened,
            what: What::Opened,
        };
        let mut events = vec![opened];
        let commits = conversation.commits.iter().enumerate();
        events.extend(commits.map(|(at, commit)| Event {
            at: commit.at,
            what: What::Commit(at),
        }));
        events.extend(conversation.places.iter().map(|&place| {
            let remark = conversation.remark(place);
            let what = match remark.said {
                Some(Said::Verdict(_)) => What::Verdict(place),
                _ => What::Remark(place),
            };
            Event {
                at: remark.at,
                what,
            }
        }));
        if let Some((ended, at)) = lifecycle.ended {
            let what = match ended {
                Ended::Merged => What::Merged,
                Ended::Closed => What::Closed,
            };
            events.push(Event { at, what });
        }
        events.sort_by_key(|event| Reverse(event.at));

        // Each run of remarks between two other events is one row.
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        for (at, event) in events.iter().enumerate() {
            let What::Remark(place) = event.what else {
                rows.push(Row::Event(at));
                continue;
            };
            match rows.last_mut() {
                Some(Row::Run(_, run)) => run.end = at + 1,
                _ => {
                    rows.push(Row::Run(keys.len(), at..at + 1));
                    keys.push(conversation.remark(place).id.clone());
                }
            }
        }
        Timeline {
            events,
            rows,
            keys,
            opener: conversation.author.clone().or(lifecycle.author.clone()),
            newest: conversation.commits.iter().filter_map(|c| c.at).max(),
            lifecycle: lifecycle.clone(),
        }
    }

    /// Whether it was laid out with `lifecycle`.
    pub(super) fn follows(&self, lifecycle: &Lifecycle) -> bool {
        self.lifecycle == *lifecycle
    }

    /// Whether a run is known by `key`.
    pub(super) fn has_key(&self, key: &str) -> bool {
        self.keys.iter().any(|known| known == key)
    }

    /// How many runs of remarks it has.
    pub(super) fn runs(&self) -> usize {
        self.keys.len()
    }

    /// What run `run` is known by while the reader opens it: its newest remark's id. A run
    /// that a newer remark joins is another run, which starts closed, as the desktop keys a
    /// run by its first remark (PullRequestTimelineTab.tsx:588). The order doesn't change it.
    pub(super) fn key(&self, run: usize) -> Option<&str> {
        self.keys.get(run).map(String::as_str)
    }

    /// Whether the reader opened run `run`.
    pub(super) fn open(&self, run: usize, shown: &Shown) -> bool {
        self.key(run)
            .is_some_and(|key| shown.runs_open.contains(key))
    }

    /// The remarks whose text shows while the reader opened what `shown` says: each verdict's,
    /// and those of each run that is open.
    pub(super) fn places_shown(&self, shown: &Shown) -> Vec<Place> {
        let mut places = Vec::new();
        for row in &self.rows {
            let events = match row {
                Row::Event(at) => &self.events[*at..*at + 1],
                Row::Run(run, events) if self.open(*run, shown) => &self.events[events.clone()],
                Row::Run(..) => continue,
            };
            places.extend(events.iter().filter_map(|event| match event.what {
                What::Verdict(place) | What::Remark(place) => Some(place),
                _ => None,
            }));
        }
        places
    }

    /// An event's rows, other than a remark's, which shows in its run.
    fn event_lines(
        &self,
        conversation: &Conversation,
        event: &Event,
        width: usize,
        now: i64,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let strong = Style::new().fg(theme.fg).add_modifier(Modifier::BOLD);
        let time = event.at.map(|at| ago(at, now));
        let (label, color) = match event.what {
            What::Opened => ("Pull request opened", theme.success),
            What::Merged => ("Pull request merged", theme.violet),
            What::Closed => ("Pull request closed", theme.error_fg),
            What::Commit(at) => return commit_lines(&conversation.commits[at], width, now, theme),
            What::Verdict(place) => {
                let remark = conversation.remark(place);
                return self.verdict_lines(remark, event.at, width, now, theme);
            }
            What::Remark(_) => return Vec::new(),
        };
        let mut title = Vec::new();
        if event.what == What::Opened
            && let Some(opener) = &self.opener
        {
            title.push((opener.clone(), strong));
        }
        title.push((label.to_string(), strong));
        let mark = Span::styled("●", Style::new().fg(color));
        entry(mark, title, time.into_iter().collect(), width, theme)
    }

    /// A verdict's rows, as the desktop's `ReviewVerdictEvent` shows it: who gave it and the
    /// verdict in its color, then when and on which file, then its text. A verdict given before
    /// the newest commit is grey and says so in the desktop's words for a screen reader.
    fn verdict_lines(
        &self,
        remark: &Remark,
        at: Option<i64>,
        width: usize,
        now: i64,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let Some(Said::Verdict(outcome)) = remark.said else {
            return Vec::new();
        };
        let stale = matches!((at, self.newest), (Some(at), Some(newest)) if at < newest);
        let mark = match outcome {
            Outcome::Approved => "✓",
            Outcome::ChangesRequested => "✗",
            Outcome::Dismissed => "–",
        };
        let color = if stale {
            theme.muted
        } else {
            outcome.color(theme)
        };
        let label = outcome.label().to_lowercase();
        let label = match stale {
            true => format!("{label}, before the latest commits"),
            false => label,
        };
        let strong = Style::new().fg(theme.fg).add_modifier(Modifier::BOLD);
        let title = vec![
            (remark.author.clone(), strong),
            (label, Style::new().fg(color)),
        ];
        let time = at.map(|at| ago(at, now));
        let meta = time.into_iter().chain(remark.location()).collect();
        let mark = Span::styled(mark, Style::new().fg(color));
        let mut lines = entry(mark, title, meta, width, theme);
        lines.extend(remark.text(width, theme));
        lines
    }
}

/// The Timeline as the conversation's read stands and the reader opened it: that the read is
/// under way or why it failed, or its header and then its rows with a blank row between each
/// two, and the rows of each control among them. Where `opens` says the details offer the
/// Code tab, a commit's rows show its own diff there, as the desktop's `CommitEvent` is a
/// button that does (PullRequestTimelineTab.tsx:369-407, PullRequestDetailPanel.tsx:516-519).
pub(in crate::tui::pull_requests) fn section(
    answer: &Answer<Conversation>,
    shown: &Shown,
    opens: bool,
    width: usize,
    now: i64,
    theme: &Theme,
) -> Section {
    let muted = Style::new().fg(theme.muted);
    let none = || Section {
        lines: Vec::new(),
        targets: Vec::new(),
    };
    let conversation = match answer {
        _ if width == 0 => return none(),
        Answer::Reading => {
            return Section {
                lines: whole("Reading the conversation…", width, muted),
                targets: Vec::new(),
            };
        }
        Answer::Failed(failure) => {
            let value = Style::new().fg(theme.fg);
            let mut lines = whole("Could not load pull request activity", width, value);
            lines.extend(whole(failure, width, muted));
            return Section {
                lines,
                targets: Vec::new(),
            };
        }
        Answer::Read(conversation) => conversation,
    };
    let Some(timeline) = conversation.timeline.as_ref() else {
        return none();
    };
    let mut rows = Rows {
        lines: Vec::new(),
        targets: Vec::new(),
        first: 0,
    };
    header(&mut rows, conversation, shown, width, theme);
    let oldest_first = shown.timeline_oldest_first;
    let mut order: Vec<&Row> = timeline.rows.iter().collect();
    if oldest_first {
        order.reverse();
    }
    for row in order {
        match row {
            Row::Event(at) => {
                let event = &timeline.events[*at];
                let lines = timeline.event_lines(conversation, event, width, now, theme);
                match event.what {
                    What::Commit(commit) if opens => {
                        rows.control(lines, Control::TimelineCommit(commit));
                    }
                    _ => rows.item(lines, None),
                }
            }
            Row::Run(run, events) => {
                let mut events: Vec<&Event> = timeline.events[events.clone()].iter().collect();
                if oldest_first {
                    events.reverse();
                }
                let open = timeline.open(*run, shown);
                let lines = run_lines(conversation, &events, open, width, now, theme);
                rows.control(lines, Control::Run(*run));
                if !open {
                    continue;
                }
                for event in events {
                    if let What::Remark(place) = event.what {
                        let remark = conversation.remark(place);
                        rows.item(card_lines(remark, width, now, theme), None);
                    }
                }
            }
        }
    }
    if conversation.cut {
        let note = "T3 sent more comments, review threads, commits or reviewers than t3term keeps.";
        rows.item(whole(note, width, muted), None);
    }
    Section {
        lines: rows.lines,
        targets: rows.targets,
    }
}

/// The Timeline's header, as the desktop's tab bar shows it beside the Timeline: how many
/// remarks the host counts, how many commits there are and how many approvals still stand,
/// then the switch between newest and oldest first at the end of the row, or on the row under
/// them where both don't fit.
fn header(
    rows: &mut Rows,
    conversation: &Conversation,
    shown: &Shown,
    width: usize,
    theme: &Theme,
) {
    let muted = Style::new().fg(theme.muted);
    let mut counts = vec![
        Span::styled(plural(conversation.count, "comment"), muted),
        Span::styled(plural(conversation.commits.len() as u64, "commit"), muted),
    ];
    let approvals = conversation.approvals() as u64;
    if approvals > 0 {
        let approved = Style::new().fg(theme.success);
        counts.push(Span::styled(plural(approvals, "approval"), approved));
    }
    let order = match shown.timeline_oldest_first {
        true => "Oldest first",
        false => "Newest first",
    };
    let at = rows.lines.len();
    let left = joined(counts.clone(), Span::styled(" · ", muted));
    let used: usize = left.iter().map(|span| span.content.width()).sum();
    if used + 1 + order.width() <= width {
        rows.lines
            .push(row(left, vec![Span::styled(order, muted)], width));
        rows.targets.push((at..at + 1, Control::TimelineOrder));
        return;
    }
    rows.lines.extend(flowed(counts, " · ", width, theme));
    let at = rows.lines.len();
    rows.lines.extend(whole(order, width, muted));
    rows.targets
        .push((at..rows.lines.len(), Control::TimelineOrder));
}

/// `3 comments`, or `1 comment`, with the count grouped.
fn plural(count: u64, what: &str) -> String {
    let s = if count == 1 { "" } else { "s" };
    format!("{} {what}{s}", grouped(count))
}

/// A commit's rows, as the desktop's `CommitEvent` shows it: its headline, or `Untitled commit`,
/// on one row with the lines it adds and removes at the end, then its id's first seven
/// characters, its first author and when it came.
fn commit_lines(commit: &Commit, width: usize, now: i64, theme: &Theme) -> Vec<Line<'static>> {
    let gutter = gutter(width);
    let strong = Style::new().fg(theme.fg).add_modifier(Modifier::BOLD);
    let headline = commit.headline();
    // No counts show where the host counted none, as the desktop's `PullRequestDiffStat`
    // leaves `+0 -0` out.
    let counts = match commit.counts {
        None | Some((0, 0)) => Vec::new(),
        Some((additions, deletions)) => vec![
            Span::styled(
                format!("+{}", grouped(additions)),
                Style::new().fg(theme.success),
            ),
            Span::raw(" "),
            Span::styled(
                format!("-{}", grouped(deletions)),
                Style::new().fg(theme.error_fg),
            ),
        ],
    };
    let counted: usize = counts.iter().map(|span| span.content.width()).sum();
    let room = width - gutter;
    // The counts show only beside at least four columns of the headline.
    let (counts, room) = match counted > 0 && 4 + 1 + counted <= room {
        true => (counts, room - 1 - counted),
        false => (Vec::new(), room),
    };
    let mut left = lead(
        Span::styled("○", Style::new().fg(theme.muted)),
        gutter,
        true,
    );
    left.push(Span::styled(clip(headline, room), strong));
    let mut lines = vec![row(left, counts, width)];
    let time = commit.at.map(|at| ago(at, now));
    let meta = [Some(commit.short.clone()), commit.author.clone(), time];
    let meta: Vec<String> = meta
        .into_iter()
        .flatten()
        .filter(|p| !p.is_empty())
        .collect();
    for spans in meta_rows(&meta, width - gutter, theme) {
        let mut line = lead(Span::raw(""), gutter, false);
        line.extend(spans);
        lines.push(Line::from(line));
    }
    lines
}

/// A run's rows, as the desktop's `ConversationGroup` shows it: a chevron and how many remarks
/// it holds, grey until it opens, then how many people wrote them and when the first in the
/// order shown came. One login in two casings is one person and a deleted account is `ghost`,
/// as the Summary's groups count them.
fn run_lines(
    conversation: &Conversation,
    events: &[&Event],
    open: bool,
    width: usize,
    now: i64,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let authors: HashSet<String> = events
        .iter()
        .filter_map(|event| match event.what {
            What::Remark(place) => Some(conversation.remark(place).author.to_lowercase()),
            _ => None,
        })
        .collect();
    let color = if open { theme.fg } else { theme.muted };
    let chevron = if open { "⌄" } else { "›" };
    let style = Style::new().fg(color).add_modifier(Modifier::BOLD);
    let title = vec![(plural(events.len() as u64, "comment"), style)];
    let mut meta = vec![plural(authors.len() as u64, "author")];
    if let Some(at) = events.first().and_then(|event| event.at) {
        meta.push(ago(at, now));
    }
    entry(
        Span::styled(chevron, Style::new().fg(color)),
        title,
        meta,
        width,
        theme,
    )
}

/// A remark's rows in an open run, as the desktop's `ConversationCard` shows it: who wrote it,
/// whether they reviewed or commented, and the review's state where it has one, then when and
/// on which file, then its reactions and its text.
fn card_lines(remark: &Remark, width: usize, now: i64, theme: &Theme) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let strong = Style::new().fg(theme.fg).add_modifier(Modifier::BOLD);
    let did = if remark.review {
        "reviewed"
    } else {
        "commented"
    };
    let mut title = vec![(remark.author.clone(), strong), (did.to_string(), muted)];
    // A space more sets the state apart, as a badge.
    if let Some(Said::State(state)) = &remark.said {
        title.push((format!(" {state}"), muted));
    }
    let time = remark.at.map(|at| ago(at, now));
    let meta = time.into_iter().chain(remark.location()).collect();
    let mut lines = entry(Span::raw(" "), title, meta, width, theme);
    let gutter = gutter(width);
    for line in remark.reacted(width - gutter, theme) {
        let mut spans = lead(Span::raw(" "), gutter, false);
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }
    lines.extend(remark.text(width, theme));
    lines
}

/// The columns an entry's mark takes at `width`: none where the details are too narrow to
/// spare them.
fn gutter(width: usize) -> usize {
    if width > GUTTER { GUTTER } else { 0 }
}

/// What starts an entry's row in the `gutter`: its `mark` and a space on the `first` row, and
/// blank columns on the rest.
fn lead(mark: Span<'static>, gutter: usize, first: bool) -> Vec<Span<'static>> {
    match (gutter, first) {
        (0, _) => Vec::new(),
        (_, true) => vec![mark, Span::raw(" ")],
        (_, false) => vec![Span::raw(" ".repeat(gutter))],
    }
}

/// An entry's rows: `mark`, then `title`'s pieces a space apart on the first row, then `meta`'s
/// parts under it, each in from the left by the mark's columns. A title too wide for its row
/// puts each piece on rows of its own, wrapped to every row it takes.
fn entry(
    mark: Span<'static>,
    title: Vec<(String, Style)>,
    meta: Vec<String>,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let gutter = gutter(width);
    let room = width - gutter;
    let gaps = title.len().saturating_sub(1);
    let used = title.iter().map(|(text, _)| text.width()).sum::<usize>() + gaps;
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    if used <= room {
        let mut spans = Vec::new();
        for (text, style) in title {
            if !spans.is_empty() {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(text, style));
        }
        rows.push(spans);
    } else {
        for (text, style) in title {
            let lines = whole(text.trim_start(), room, style);
            rows.extend(lines.into_iter().map(|line| line.spans));
        }
    }
    rows.extend(meta_rows(&meta, room, theme));
    rows.into_iter()
        .enumerate()
        .map(|(at, spans)| {
            let mut line = lead(mark.clone(), gutter, at == 0);
            line.extend(spans);
            Line::from(line)
        })
        .collect()
}

/// `meta`'s parts in grey, a dot between each two on a row. A part that doesn't fit beside the
/// others starts a row, so `1d ago` stays whole.
fn meta_rows(meta: &[String], room: usize, theme: &Theme) -> Vec<Vec<Span<'static>>> {
    let style = Style::new().fg(theme.muted);
    let parts = meta
        .iter()
        .map(|part| Span::styled(part.clone(), style))
        .collect();
    let lines = flowed(parts, " · ", room, theme);
    lines.into_iter().map(|line| line.spans).collect()
}
