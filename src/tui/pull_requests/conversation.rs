//! A pull request's conversation in a link's details, from T3's `pullRequests.activity`, as the
//! desktop's Summary tab shows it under Comments and beside each reviewer
//! (apps/web/src/components/pullRequest/PullRequestSummaryTab.tsx and pullRequestDetail.logic.ts
//! at f570bd2).
//!
//! The remarks split as the desktop splits them. One in a resolved review thread, or a
//! dismissed review, is finished. Otherwise one whose author is a bot, or whose login ends in
//! `[bot]`, is a bot's. The rest are active, and the 10 most recent of those show, newest
//! first. Each reviewer shows their last verdict, read from the whole conversation, and a
//! verdict older than the newest commit reads as given on earlier changes.
//!
//! The section's controls do what the desktop's buttons do: they show 10 older remarks at a
//! time and go back to the 10 most recent, switch between newest and oldest first, and open
//! the bots' remarks, the finished ones and each finished one. A finished remark shows on one
//! row until it opens. `Shown` keeps what the reader opened until the details open again.
//!
//! The same remarks, with the commits and the pull request's opening, merge or close, make up
//! the Timeline, which `timeline` lays out.
//!
//! The review threads, each a conversation on a line of the diff, show on the Code tab, which
//! places them. Each is a `Thread` of remarks, open unless resolved until the reader opens or
//! closes it, as the desktop's `ReviewThreadCard` starts. Where the host has more remarks than
//! the activity sent, the reader can read them a page at a time, as the card's `Load more
//! comments` does. The pages a thread read go with it when the details read the conversation
//! again.
//!
//! What shows is cleaned and bounded once, when the answer arrives. Each remark's Markdown is
//! laid out once for each width, and only while it shows.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::super::markdown::{self, Styles};
use super::super::setup::bounded;
use super::super::theme::{Theme, age, parse_iso_ms};
use super::super::{clip, row};
use super::{Answer, FAILURE, NAME, flowed, grouped, whole};
use crate::pull_request::{
    self, Actor, Comment, CommentKind, Reaction, ReactionContent, ReviewThread, Side, ThreadComment,
};
use crate::transcript::without_controls;

mod thread;
mod timeline;

use timeline::Timeline;
pub(super) use timeline::{Commit, Lifecycle, section as timeline_section};

/// How many active remarks show, as the desktop's first page of the conversation holds
/// (`COMMENT_PAGE`).
const COMMENT_PAGE: usize = 10;
/// The most characters kept of a file a remark is on.
const PATH: usize = 1024;
/// The columns a remark's text is set in from the left, where the details are wide enough to
/// spare them.
const MARGIN: usize = 2;
/// The narrowest details a remark's text is set in at.
const INDENTED: usize = 20;
/// The most characters kept of a finished remark's row.
const PREVIEW: usize = 1024;

/// What the reader opened of a pull request's conversation, which the desktop keeps for one
/// pull request at a time: the order, how many active and bots' remarks show, and which
/// groups and finished remarks are open. Then the Timeline's own order and which of its runs
/// of remarks are open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Shown {
    oldest_first: bool,
    /// How many of the most recent active remarks show.
    active: usize,
    bots_open: bool,
    /// How many of the most recent bots' remarks show while their group is open.
    bots: usize,
    finished_open: bool,
    /// The finished remarks open, by id.
    opened: HashSet<String>,
    timeline_oldest_first: bool,
    /// The Timeline's runs of remarks that are open, by `Timeline::key`.
    runs_open: HashSet<String>,
    /// The review threads the reader opened or closed on the Code tab, by id, and whether
    /// they are open. The others are open unless resolved.
    threads: HashMap<String, bool>,
    /// The Code tab's list of the conversations off the diff is open.
    off_diff_open: bool,
}

impl Default for Shown {
    fn default() -> Shown {
        Shown {
            oldest_first: false,
            active: COMMENT_PAGE,
            bots_open: false,
            bots: COMMENT_PAGE,
            finished_open: false,
            opened: HashSet::new(),
            timeline_oldest_first: false,
            runs_open: HashSet::new(),
            threads: HashMap::new(),
            off_diff_open: false,
        }
    }
}

impl Shown {
    /// Does what `control` does to what shows of `conversation`.
    pub(super) fn act(&mut self, control: Control, conversation: &Conversation) {
        match control {
            Control::Order => self.oldest_first = !self.oldest_first,
            Control::Older => self.active = more(self.active, conversation.active.len()),
            Control::Recent => self.active = COMMENT_PAGE,
            Control::Bots => self.bots_open = !self.bots_open,
            Control::OlderBots => self.bots = more(self.bots, conversation.bots.len()),
            Control::RecentBots => self.bots = COMMENT_PAGE,
            Control::Finished => self.finished_open = !self.finished_open,
            Control::Remark(at) => {
                if let Some(remark) = conversation.finished.get(at)
                    && !self.opened.remove(&remark.id)
                {
                    self.opened.insert(remark.id.clone());
                }
            }
            Control::TimelineOrder => self.timeline_oldest_first = !self.timeline_oldest_first,
            Control::Run(at) => {
                let timeline = conversation.timeline.as_ref();
                if let Some(key) = timeline.and_then(|timeline| timeline.key(at))
                    && !self.runs_open.remove(key)
                {
                    self.runs_open.insert(key.to_string());
                }
            }
            Control::Thread(at) => {
                if let Some(thread) = conversation.threads.get(at) {
                    let open = self.thread_open(thread);
                    self.threads.insert(thread.id.clone(), !open);
                }
            }
            Control::OffDiff => self.off_diff_open = !self.off_diff_open,
            Control::File(_)
            | Control::Whitespace
            | Control::Folds
            | Control::Stretch(..)
            | Control::Retry
            | Control::More(_)
            | Control::Commits
            | Control::AllCommits
            | Control::Commit(_)
            | Control::MoreCommits
            | Control::TimelineCommit(_) => {}
        }
    }

    /// Closes the Code tab's list of the conversations off the diff, as the desktop does when
    /// the reader picks another commit's diff (PullRequestCodeTab.tsx:268-277).
    pub(super) fn close_off_diff(&mut self) {
        self.off_diff_open = false;
    }

    /// Whether `thread` shows its remarks: as the reader left it, or else unless resolved.
    pub(super) fn thread_open(&self, thread: &Thread) -> bool {
        self.threads
            .get(&thread.id)
            .copied()
            .unwrap_or(!thread.resolved)
    }

    /// Whether the Code tab's list of the conversations off the diff is open.
    pub(super) fn off_diff_open(&self) -> bool {
        self.off_diff_open
    }

    /// Forgets the finished remarks and the Timeline's runs that were open and that
    /// `conversation` no longer has, so what is kept stays within its size however often the
    /// details read it again.
    pub(super) fn prune(&mut self, conversation: &Conversation) {
        let finished: HashSet<&str> = conversation
            .finished
            .iter()
            .map(|remark| remark.id.as_str())
            .collect();
        self.opened.retain(|id| finished.contains(id.as_str()));
        if let Some(timeline) = conversation.timeline.as_ref() {
            self.runs_open.retain(|key| timeline.has_key(key));
        }
        let threads: HashSet<&str> = conversation
            .threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect();
        self.threads.retain(|id, _| threads.contains(id.as_str()));
    }
}

/// `shown` and a page more, no more than `total` or a page.
fn more(shown: usize, total: usize) -> usize {
    shown
        .saturating_add(COMMENT_PAGE)
        .min(total.max(COMMENT_PAGE))
}

/// A row of the Comments section that does something when it is chosen or clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Control {
    /// The switch between newest and oldest first, at the end of the section's heading.
    Order,
    /// Shows up to 10 older active remarks.
    Older,
    /// Shows only the 10 most recent active remarks again.
    Recent,
    /// Opens or closes the bots' remarks.
    Bots,
    OlderBots,
    RecentBots,
    /// Opens or closes the resolved and dismissed remarks.
    Finished,
    /// Opens or closes one finished remark, by its place among them as T3 sent them.
    Remark(usize),
    /// The Timeline's switch between newest and oldest first.
    TimelineOrder,
    /// Opens or closes a run of remarks on the Timeline, by its place among the runs newest
    /// first.
    Run(usize),
    /// Unfolds or folds a file on the Code tab, by its place among the files shown. `code`
    /// acts on it, as on `Retry`.
    File(usize),
    /// Shows the changes to whitespace on the Code tab, or hides them again.
    Whitespace,
    /// Unfolds every file of the Code tab, or folds them all once one shows unfolded.
    Folds,
    /// Reads again the slice of the diff whose read failed, at the end of the Code tab.
    Retry,
    /// Opens or closes a review thread on the Code tab, by its place among the threads as T3
    /// sent them.
    Thread(usize),
    /// Opens or closes the Code tab's list of the conversations off the diff.
    OffDiff,
    /// Reads the next page of a review thread's remarks, by the thread's place as for
    /// `Thread`.
    More(usize),
    /// Opens or closes the Code tab's list of commits, which picks the diff it shows.
    Commits,
    /// Shows the diff of the whole change on the Code tab, from its list of commits.
    AllCommits,
    /// Shows a commit's own diff on the Code tab, by its place among the commits newest
    /// first.
    Commit(usize),
    /// Lists up to 10 more commits in the Code tab's list.
    MoreCommits,
    /// Shows a commit's own diff on the Code tab, from its row on the Timeline, by its place
    /// among the commits as T3 sent them.
    TimelineCommit(usize),
    /// Shows more of a run of unchanged lines on the Code tab, by its file's place as for
    /// `File` and its place among the file's runs: one before each hunk, then one after the
    /// last.
    Stretch(usize, usize),
}

impl Control {
    /// Whether the Code tab's own state, rather than the conversation's, says what it does:
    /// a file, the toggle for the whitespace, the button that folds or unfolds every file, a
    /// run of unchanged lines, Retry and the list of commits.
    pub(super) fn on_code(self) -> bool {
        matches!(
            self,
            Control::File(_)
                | Control::Whitespace
                | Control::Folds
                | Control::Stretch(..)
                | Control::Retry
        ) || self.on_commits()
    }

    /// Whether it is the list of commits, which stays above the files when a diff comes.
    pub(super) fn on_commits(self) -> bool {
        matches!(
            self,
            Control::Commits | Control::AllCommits | Control::Commit(_) | Control::MoreCommits
        )
    }

    /// The control that takes this one's place when it goes: Show only 10 recent once every
    /// older remark shows, and Show older once only 10 do.
    pub(super) fn partner(self) -> Option<Control> {
        match self {
            Control::Older => Some(Control::Recent),
            Control::Recent => Some(Control::Older),
            Control::OlderBots => Some(Control::RecentBots),
            Control::RecentBots => Some(Control::OlderBots),
            _ => None,
        }
    }
}

/// A verdict a review gives. `PullRequestReviewOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Approved,
    ChangesRequested,
    Dismissed,
}

impl Outcome {
    /// The verdict a review's state gives, as `pullRequestReviewOutcome` reads it. Hosts spell
    /// the same verdict differently, such as GitHub's `CHANGES_REQUESTED` and Bitbucket's
    /// `changes_requested`, so case and separator don't count. Any other state, such as
    /// GitHub's `COMMENTED`, is no verdict.
    fn of(state: &str) -> Option<Outcome> {
        match state.trim().to_lowercase().replace('_', "-").as_str() {
            "approved" => Some(Outcome::Approved),
            "changes-requested" => Some(Outcome::ChangesRequested),
            "dismissed" => Some(Outcome::Dismissed),
            _ => None,
        }
    }

    /// Its name, as the desktop's `REVIEW_OUTCOME_PRESENTATION` gives it.
    fn label(self) -> &'static str {
        match self {
            Outcome::Approved => "Approved",
            Outcome::ChangesRequested => "Changes requested",
            Outcome::Dismissed => "Review dismissed",
        }
    }

    fn color(self, theme: &Theme) -> Color {
        match self {
            Outcome::Approved => theme.success,
            Outcome::ChangesRequested => theme.error_fg,
            Outcome::Dismissed => theme.muted,
        }
    }
}

/// A person a review was asked of, or who reviewed.
pub(super) struct Reviewer {
    /// Their login, cleaned and bounded.
    login: String,
    /// Their login as the host sent it, lowercased. A host doesn't keep one casing of a login
    /// across two fields, and none lets `Octocat` and `octocat` be two people.
    key: String,
    bot: bool,
}

impl Reviewer {
    pub(super) fn of(actor: &Actor) -> Reviewer {
        Reviewer {
            login: bounded(&actor.login, NAME),
            key: actor.login.to_lowercase(),
            bot: actor.is_bot,
        }
    }

    fn label(&self) -> String {
        match self.bot {
            true => format!("{} (bot)", self.login),
            false => self.login.clone(),
        }
    }
}

/// A reviewer's last verdict. `PullRequestReviewOutcomeEntry`.
struct Verdict {
    /// Who gave it. None for a review with no author, which stands alone.
    reviewer: Option<Reviewer>,
    outcome: Outcome,
    /// Given before the newest commit, so on changes the branch has moved past.
    stale: bool,
}

impl Verdict {
    fn label(&self) -> String {
        match self.stale {
            true => format!("{} earlier changes", self.outcome.label()),
            false => self.outcome.label().to_string(),
        }
    }

    fn color(&self, theme: &Theme) -> Color {
        match self.stale {
            true => theme.muted,
            false => self.outcome.color(theme),
        }
    }
}

/// What a remark says beside its author: a verdict, or the review's state in words.
enum Said {
    Verdict(Outcome),
    /// `CHANGES_REQUESTED` reads as `Changes requested`, as `reviewStateLabel` writes it.
    State(String),
}

/// A review thread: a conversation on a line of the diff, or on a file, cleaned and bounded.
/// `PullRequestReviewThread`, as the desktop's `ReviewThreadCard` shows it.
pub(super) struct Thread {
    id: String,
    /// The file it is on, as the host names it, which is how the diff names it too.
    pub(super) path: String,
    /// `None` where the host anchors it to the file rather than a line.
    pub(super) line: Option<u64>,
    /// Whether `line` counts lines before the change or after it.
    pub(super) side: Side,
    resolved: bool,
    /// The line it was written on is no longer in the diff.
    outdated: bool,
    /// How many remarks the host counts, at least as many as `remarks` holds.
    count: u64,
    /// Oldest first, as T3 sends them.
    remarks: Vec<Remark>,
    /// The remarks pages read after those, oldest first, none of them one `remarks` has.
    loaded: Vec<Remark>,
    /// Where the next page of remarks starts: the activity's cursor until a page came, then
    /// the last page's. `None` once the thread is whole, or holds as many remarks as t3term
    /// keeps.
    next: Option<String>,
    /// Whether a page came, so `next` is a page's cursor.
    paged: bool,
    /// Where the read of the next page is.
    more: More,
}

/// Where the read of a thread's next page of remarks is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) enum More {
    #[default]
    Idle,
    /// The reader asked for it, and the next read sends it.
    Wanted,
    /// The read with this ticket is under way.
    Asked(u64),
    /// The last read failed, for this reason.
    Failed(String),
}

/// One remark, cleaned and bounded.
struct Remark {
    id: String,
    /// Whether it is a submitted review rather than a comment.
    review: bool,
    /// The author's login, or `ghost` for a deleted account.
    author: String,
    /// When it was written, in milliseconds. None where the host's time doesn't parse.
    at: Option<i64>,
    said: Option<Said>,
    /// The file and line it is on: its review thread's, or else its own file.
    location: Option<String>,
    /// Its review thread's line is no longer in the diff.
    outdated: bool,
    /// Its text without control characters or HTML comments. None when nothing is left, as
    /// a review that only gives a verdict usually has.
    body: Option<String>,
    /// Whether T3 sent a longer remark than t3term keeps.
    body_cut: bool,
    /// Its review thread is resolved, so a finished remark reads Resolved rather than Review
    /// dismissed.
    resolved: bool,
    /// A finished remark's text on one row, as the desktop shows it until the remark opens.
    preview: String,
    /// Its reactions, in the order T3 sent them.
    reactions: Vec<Pill>,
    /// The text laid out at one width.
    laid_out: Option<(usize, Vec<Line<'static>>)>,
}

/// A reaction on a remark as the desktop's `PullRequestReactionBar` shows it: its emoji and how
/// many people reacted with it.
struct Pill {
    text: String,
    /// The reader reacted with it, which the desktop marks out.
    mine: bool,
}

impl Pill {
    fn of(reaction: &Reaction) -> Pill {
        Pill {
            text: format!("{} {}", emoji(reaction.content), reaction.count),
            mine: reaction.viewer_reacted,
        }
    }
}

/// A reaction's emoji, as the desktop's `REACTION_EMOJI` gives them
/// (pullRequestReactions.logic.ts), but for the heart. The desktop's ❤️ is ❤ and U+FE0F, which
/// some terminals draw a column wide and others two, and ratatui 0.30 writes the cell after it
/// without moving the cursor. Where the terminal draws it two wide, the rest of the row then
/// lands a column to the right. Without the selector, each one's width is its East Asian Width,
/// as unicode-width counts it: one for the heart and two for the other seven.
fn emoji(content: ReactionContent) -> &'static str {
    match content {
        ReactionContent::ThumbsUp => "👍",
        ReactionContent::ThumbsDown => "👎",
        ReactionContent::Laugh => "😄",
        ReactionContent::Hooray => "🎉",
        ReactionContent::Confused => "😕",
        ReactionContent::Heart => "❤",
        ReactionContent::Rocket => "🚀",
        ReactionContent::Eyes => "👀",
    }
}

/// What a group of remarks says of them while it is closed, as the desktop's `CommentGroup`
/// does: how many people wrote them, on how many files, and when the latest came.
struct Group {
    authors: usize,
    files: usize,
    latest: Option<i64>,
}

impl Group {
    fn of(remarks: &[(&Comment, Option<&ReviewThread>)]) -> Group {
        // A deleted account is `ghost`, and one login in two casings is one person.
        let authors: HashSet<String> = remarks
            .iter()
            .map(|(comment, _)| {
                let author = comment.author.as_ref();
                author.map_or_else(|| "ghost".to_string(), |actor| actor.login.to_lowercase())
            })
            .collect();
        let files: HashSet<&str> = remarks
            .iter()
            .filter_map(|(comment, _)| comment.path.as_deref().filter(|path| !path.is_empty()))
            .collect();
        let latest = remarks
            .iter()
            .filter_map(|(comment, _)| instant(&comment.created_at))
            .max();
        Group {
            authors: authors.len(),
            files: files.len(),
            latest,
        }
    }

    /// Its second row: `2 authors · 1 file · Latest 5m ago`.
    fn line(&self, now: i64) -> String {
        let plural = |count: usize, what: &str| {
            let s = if count == 1 { "" } else { "s" };
            format!("{} {what}{s}", grouped(count as u64))
        };
        let mut parts = vec![plural(self.authors, "author")];
        if self.files > 0 {
            parts.push(plural(self.files, "file"));
        }
        if let Some(latest) = self.latest {
            parts.push(format!("Latest {}", ago(latest, now)));
        }
        parts.join(" · ")
    }
}

/// What the details show of a pull request's conversation.
pub(super) struct Conversation {
    /// How many remarks the host counts, which the section's label shows.
    count: u64,
    /// T3 read only the most recent remarks.
    truncated: bool,
    /// How many remarks T3 sent.
    sent: usize,
    /// Each group's remarks in the order T3 sent them, the oldest first.
    active: Vec<Remark>,
    bots: Vec<Remark>,
    bots_group: Group,
    finished: Vec<Remark>,
    finished_group: Group,
    /// The reviewers, where the host gave them with the conversation. They take the place of
    /// the detail's.
    reviewers: Option<Vec<Reviewer>>,
    verdicts: Vec<Verdict>,
    /// T3 sent more remarks, review threads, commits or reviewers than t3term keeps.
    cut: bool,
    /// Where each remark went among the groups above, in the order T3 sent them.
    places: Vec<Place>,
    /// The commits, in the order T3 sent them.
    commits: Vec<timeline::Commit>,
    /// The places of the commits, newest first, as the Code tab lists them.
    newest: Vec<usize>,
    /// The author's login, where the host gave it with the conversation. It takes the place of
    /// the detail's.
    author: Option<String>,
    /// The Timeline, once the pull request's detail came too.
    timeline: Option<Timeline>,
    /// The review threads, in the order T3 sent them.
    threads: Vec<Thread>,
}

/// Where a remark is among a conversation's groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Active(usize),
    Bot(usize),
    Finished(usize),
}

impl Conversation {
    pub(super) fn of(activity: &pull_request::Activity) -> Conversation {
        // A remark on a review thread takes the thread's file, line and resolution. A later
        // thread naming the same remark wins, as the desktop's map lets it.
        let threads: HashMap<&str, &ReviewThread> = activity
            .threads
            .iter()
            .flat_map(|thread| thread.comments.iter().map(move |c| (c.id.as_str(), thread)))
            .collect();
        let (mut active, mut bots, mut finished) = (Vec::new(), Vec::new(), Vec::new());
        let mut places = Vec::with_capacity(activity.comments.len());
        for comment in &activity.comments {
            let thread = threads.get(comment.id.as_str()).copied();
            let outcome = comment.review_state.as_deref().and_then(Outcome::of);
            let author = comment.author.as_ref();
            if thread.is_some_and(|thread| thread.resolved) || outcome == Some(Outcome::Dismissed) {
                places.push(Place::Finished(finished.len()));
                finished.push((comment, thread));
            } else if author.is_some_and(|actor| actor.is_bot || actor.login.ends_with("[bot]")) {
                places.push(Place::Bot(bots.len()));
                bots.push((comment, thread));
            } else {
                places.push(Place::Active(active.len()));
                active.push((comment, thread));
            }
        }
        let remarks = |group: &[(&Comment, Option<&ReviewThread>)], finished: bool| {
            group
                .iter()
                .map(|(comment, thread)| Remark::of(comment, *thread, finished))
                .collect()
        };
        let reviewers = activity
            .reviewers
            .as_ref()
            .map(|reviewers| reviewers.iter().map(Reviewer::of).collect());
        let commits: Vec<timeline::Commit> =
            activity.commits.iter().map(timeline::Commit::of).collect();
        // By when each was committed, not by its text, as the desktop sorts them
        // (PullRequestCodeTab.tsx:683-692). Commits at the same instant keep the order T3 sent
        // them in, and one with no time T3 could read goes last.
        let mut newest: Vec<usize> = (0..commits.len()).collect();
        newest.sort_by_key(|&at| std::cmp::Reverse(commits[at].at));
        Conversation {
            count: activity.comment_count,
            truncated: activity.comments_truncated,
            sent: activity.comments.len(),
            active: remarks(&active, false),
            bots: remarks(&bots, false),
            bots_group: Group::of(&bots),
            finished: remarks(&finished, true),
            finished_group: Group::of(&finished),
            reviewers,
            verdicts: verdicts(&activity.comments, &activity.commits),
            cut: activity.cut,
            places,
            commits,
            newest,
            author: activity
                .author
                .as_ref()
                .map(|actor| bounded(&actor.login, NAME)),
            timeline: None,
            threads: activity.threads.iter().map(Thread::of).collect(),
        }
    }

    /// The review threads, in the order T3 sent them.
    pub(super) fn threads(&self) -> &[Thread] {
        &self.threads
    }

    /// The commits, newest first, as the Code tab's list of commits shows them.
    pub(super) fn commits(&self) -> Vec<&Commit> {
        self.newest.iter().map(|&at| &self.commits[at]).collect()
    }

    /// The commit at `at` among the commits as T3 sent them, as the Timeline knows it.
    pub(super) fn commit(&self, at: usize) -> Option<&Commit> {
        self.commits.get(at)
    }

    /// Lays the text of each remark of the threads at `at` out at `width`, unless it already
    /// is. The Code tab says which threads show.
    pub(super) fn lay_out_threads(&mut self, at: &[usize], width: usize, theme: &Theme) {
        for &at in at {
            if let Some(thread) = self.threads.get_mut(at) {
                for remark in thread.remarks.iter_mut().chain(&mut thread.loaded) {
                    remark.lay_out(thread::inner(width), theme);
                }
            }
        }
    }

    /// Takes from `old`, the conversation this one replaces, the pages of remarks each thread
    /// read and where each thread's read of the next is, as the desktop's card keeps the
    /// pages it loaded while the activity reads again.
    pub(super) fn carry(&mut self, old: &mut Conversation) {
        let olds: HashMap<String, usize> = old
            .threads
            .iter()
            .enumerate()
            .map(|(at, thread)| (thread.id.clone(), at))
            .collect();
        for thread in &mut self.threads {
            if let Some(&at) = olds.get(&thread.id) {
                thread.carry(&mut old.threads[at]);
            }
        }
    }

    /// Asks for the next page of the remarks of the thread at `at`, unless it is whole or a
    /// read of it is under way. Says whether it asked.
    pub(super) fn want_more(&mut self, at: usize) -> bool {
        let Some(thread) = self.threads.get_mut(at) else {
            return false;
        };
        if !thread.offers_more() {
            return false;
        }
        thread.more = More::Wanted;
        true
    }

    /// The id and cursor of the first thread whose next page is wanted, which the read with
    /// `ticket` is to read.
    pub(super) fn take_wanted(&mut self, ticket: u64) -> Option<(String, String)> {
        for thread in &mut self.threads {
            if thread.more != More::Wanted {
                continue;
            }
            // A read again of the conversation can leave a wanted thread with no page to read.
            let Some(cursor) = thread.next.clone() else {
                thread.more = More::Idle;
                continue;
            };
            thread.more = More::Asked(ticket);
            return Some((thread.id.clone(), cursor));
        }
        None
    }

    /// Takes the answer to the read with `ticket` of a thread's next page, or why it failed.
    /// Says whether a thread asked for it.
    pub(super) fn take_page(
        &mut self,
        ticket: u64,
        answer: Result<&pull_request::CommentPage, &str>,
    ) -> bool {
        let Some(thread) = self
            .threads
            .iter_mut()
            .find(|thread| thread.more == More::Asked(ticket))
        else {
            return false;
        };
        match answer {
            Ok(page) => thread.take(page),
            Err(failure) => thread.more = More::Failed(bounded(failure, FAILURE)),
        }
        true
    }

    /// Lays the Timeline out from the pull request's `lifecycle`, from its detail, unless it
    /// already is. A read again that brings a new detail and keeps this conversation, as when
    /// the conversation's read failed, lays it out again with the new opening and ending.
    pub(super) fn follow(&mut self, lifecycle: &Lifecycle) {
        let follows = self.timeline.as_ref();
        if !follows.is_some_and(|timeline| timeline.follows(lifecycle)) {
            self.timeline = Some(Timeline::of(lifecycle, self));
        }
    }

    fn remark(&self, place: Place) -> &Remark {
        match place {
            Place::Active(at) => &self.active[at],
            Place::Bot(at) => &self.bots[at],
            Place::Finished(at) => &self.finished[at],
        }
    }

    fn remark_mut(&mut self, place: Place) -> &mut Remark {
        match place {
            Place::Active(at) => &mut self.active[at],
            Place::Bot(at) => &mut self.bots[at],
            Place::Finished(at) => &mut self.finished[at],
        }
    }

    /// Lays the text of each remark the Timeline shows out at `width`, unless it already is.
    pub(super) fn lay_out_timeline(&mut self, width: usize, theme: &Theme, shown: &Shown) {
        let Some(timeline) = self.timeline.as_ref() else {
            return;
        };
        for place in timeline.places_shown(shown) {
            self.remark_mut(place).lay_out(width, theme);
        }
    }

    /// How many reviewers' last verdicts approve changes the branch still has, as the
    /// desktop's header counts them. None are counted from a conversation T3 read only the
    /// recent end of, since an approval older than that would be missing.
    fn approvals(&self) -> usize {
        if self.truncated {
            return 0;
        }
        let approves = |verdict: &&Verdict| verdict.outcome == Outcome::Approved && !verdict.stale;
        self.verdicts.iter().filter(approves).count()
    }

    /// Lays the text of each remark that shows out at `width`, unless it already is.
    pub(super) fn lay_out(&mut self, width: usize, theme: &Theme, shown: &Shown) {
        let from = self.active.len().saturating_sub(shown.active);
        for remark in &mut self.active[from..] {
            remark.lay_out(width, theme);
        }
        if shown.bots_open {
            let from = self.bots.len().saturating_sub(shown.bots);
            for remark in &mut self.bots[from..] {
                remark.lay_out(width, theme);
            }
        }
        if shown.finished_open {
            for remark in &mut self.finished {
                if shown.opened.contains(&remark.id) {
                    remark.lay_out(width, theme);
                }
            }
        }
    }

    /// Whether `control` shows while the reader opened what `shown` says.
    pub(super) fn has(&self, control: Control, shown: &Shown) -> bool {
        let some = self.sent > 0;
        match control {
            Control::Order => some,
            Control::Older => self.active.len() > shown.active,
            Control::Recent => some && shown.active > COMMENT_PAGE,
            Control::Bots => !self.bots.is_empty(),
            Control::OlderBots => shown.bots_open && self.bots.len() > shown.bots,
            Control::RecentBots => {
                shown.bots_open && !self.bots.is_empty() && shown.bots > COMMENT_PAGE
            }
            Control::Finished => !self.finished.is_empty(),
            Control::Remark(at) => shown.finished_open && at < self.finished.len(),
            Control::TimelineOrder => self.timeline.is_some(),
            Control::Run(at) => self.timeline.as_ref().is_some_and(|t| at < t.runs()),
            Control::TimelineCommit(at) => self.timeline.is_some() && at < self.commits.len(),
            Control::File(_)
            | Control::Whitespace
            | Control::Folds
            | Control::Stretch(..)
            | Control::Retry
            | Control::Thread(_)
            | Control::OffDiff
            | Control::More(_)
            | Control::Commits
            | Control::AllCommits
            | Control::Commit(_)
            | Control::MoreCommits => false,
        }
    }

    /// What Enter does with `control`, for the status line: `show 10 older`.
    pub(super) fn action(&self, control: Control, shown: &Shown) -> String {
        let older = |total: usize, showing: usize| {
            let hidden = total.saturating_sub(showing);
            format!("show {} older", grouped(hidden.min(COMMENT_PAGE) as u64))
        };
        let toggle = |open: bool, what: &str| match open {
            true => format!("hide {what}"),
            false => format!("show {what}"),
        };
        match control {
            Control::Order if shown.oldest_first => "newest first".to_string(),
            Control::Order => "oldest first".to_string(),
            Control::Older => older(self.active.len(), shown.active),
            Control::OlderBots => older(self.bots.len(), shown.bots),
            Control::Recent | Control::RecentBots => format!("show only {COMMENT_PAGE} recent"),
            Control::Bots => toggle(shown.bots_open, "bot comments"),
            Control::Finished => toggle(shown.finished_open, "resolved"),
            Control::Remark(at) => {
                let open = self
                    .finished
                    .get(at)
                    .is_some_and(|remark| shown.opened.contains(&remark.id));
                toggle(open, "comment")
            }
            Control::TimelineOrder if shown.timeline_oldest_first => "newest first".to_string(),
            Control::TimelineOrder => "oldest first".to_string(),
            Control::Run(at) => {
                let open = self.timeline.as_ref().is_some_and(|t| t.open(at, shown));
                toggle(open, "comments")
            }
            Control::Thread(at) => {
                let open = self
                    .threads
                    .get(at)
                    .is_some_and(|thread| shown.thread_open(thread));
                toggle(open, "conversation")
            }
            Control::OffDiff => toggle(shown.off_diff_open, "conversations off the diff"),
            Control::More(_) => "load more comments".to_string(),
            Control::TimelineCommit(at) => match self.commits.get(at) {
                Some(commit) => format!("view commit {}", commit.short),
                None => "view commit".to_string(),
            },
            Control::File(_)
            | Control::Whitespace
            | Control::Folds
            | Control::Stretch(..)
            | Control::Retry
            | Control::Commits
            | Control::AllCommits
            | Control::Commit(_)
            | Control::MoreCommits => String::new(),
        }
    }
}

impl Remark {
    /// The remark `comment` in a review thread, which the thread's card shows under the line
    /// it is on.
    fn of_thread(comment: &ThreadComment) -> Remark {
        let clean = without_controls(&comment.body.text);
        let text = without_html_comments(&clean);
        let text = text.trim();
        Remark {
            id: comment.id.clone(),
            review: false,
            author: comment
                .author
                .as_ref()
                .map_or_else(|| "ghost".to_string(), |actor| bounded(&actor.login, NAME)),
            at: instant(&comment.created_at),
            said: None,
            location: None,
            outdated: false,
            body: (!text.is_empty()).then(|| text.to_string()),
            body_cut: comment.body.cut,
            resolved: false,
            preview: String::new(),
            reactions: comment.reactions.iter().flatten().map(Pill::of).collect(),
            laid_out: None,
        }
    }

    /// The remark `comment`, on review thread `thread` where it is on one. Only a `finished`
    /// one keeps its text on one row as well.
    fn of(comment: &Comment, thread: Option<&ReviewThread>, finished: bool) -> Remark {
        let said = comment.review_state.as_deref().and_then(|state| {
            if let Some(outcome) = Outcome::of(state) {
                return Some(Said::Verdict(outcome));
            }
            let label = bounded(&state_label(state), NAME);
            (!label.is_empty()).then_some(Said::State(label))
        });
        let path = match thread {
            Some(thread) => Some(thread.path.as_str()),
            None => comment.path.as_deref(),
        };
        let location = path.filter(|path| !path.is_empty()).map(|path| {
            let path = bounded(path, PATH);
            match thread.and_then(|thread| thread.line) {
                Some(line) => format!("{path}:{line}"),
                None => path,
            }
        });
        let clean = without_controls(&comment.body.text);
        let text = without_html_comments(&clean);
        let text = text.trim();
        let preview = match finished {
            true => bounded(&preview(&clean), PREVIEW),
            false => String::new(),
        };
        Remark {
            id: comment.id.clone(),
            review: comment.kind == CommentKind::Review,
            author: comment
                .author
                .as_ref()
                .map_or_else(|| "ghost".to_string(), |actor| bounded(&actor.login, NAME)),
            at: instant(&comment.created_at),
            said,
            location,
            outdated: thread.is_some_and(|thread| thread.outdated),
            body: (!text.is_empty()).then(|| text.to_string()),
            body_cut: comment.body.cut,
            resolved: thread.is_some_and(|thread| thread.resolved),
            preview,
            reactions: comment.reactions.iter().flatten().map(Pill::of).collect(),
            laid_out: None,
        }
    }

    fn lay_out(&mut self, width: usize, theme: &Theme) {
        if self.laid_out.as_ref().is_some_and(|(at, _)| *at == width) {
            return;
        }
        let lines = match self.body.as_deref() {
            Some(body) => {
                let styles = Styles::new(theme, Style::new().fg(theme.fg));
                let margin = if width >= INDENTED { MARGIN } else { 0 };
                markdown::render(body, width, &styles, margin)
            }
            None => Vec::new(),
        };
        self.laid_out = Some((width, lines));
    }

    /// Its rows: its header, its reactions, the file and line it is on, and its text.
    fn lines(&self, width: usize, now: i64, theme: &Theme) -> Vec<Line<'static>> {
        let muted = Style::new().fg(theme.muted);
        let said = match &self.said {
            Some(Said::Verdict(outcome)) => Some((
                outcome.label().to_string(),
                Style::new().fg(outcome.color(theme)),
            )),
            Some(Said::State(state)) => Some((state.clone(), muted)),
            None => None,
        };
        let mut lines = self.header(said, width, now, theme);
        lines.extend(self.reacted(width, theme));
        lines.extend(self.place(width, theme));
        lines.extend(self.text(width, theme));
        lines
    }

    /// A finished remark's rows, as the desktop's `CollapsedComment` shows it: its header with
    /// why it is finished, its reactions, the file and line it is on, then its text on one row
    /// while it is closed and its whole text once it opens. The second number is how many of the
    /// rows open and close it: all of them while it is closed, and its header once it opens.
    fn finished_lines(
        &self,
        open: bool,
        width: usize,
        now: i64,
        theme: &Theme,
    ) -> (Vec<Line<'static>>, usize) {
        let muted = Style::new().fg(theme.muted);
        let why = if self.resolved {
            "Resolved"
        } else {
            "Review dismissed"
        };
        let chevron = if open { "⌄" } else { "›" };
        let mut lines = self.header(Some((format!("{chevron} {why}"), muted)), width, now, theme);
        let header = lines.len();
        lines.extend(self.reacted(width, theme));
        lines.extend(self.place(width, theme));
        if open {
            lines.extend(self.text(width, theme));
            return (lines, header);
        }
        if !self.preview.is_empty() && width > 0 {
            lines.push(Line::styled(clip(&self.preview, width), muted));
        }
        let rows = lines.len();
        (lines, rows)
    }

    /// The author, `said` after them, and how long ago at the end.
    fn header(
        &self,
        said: Option<(String, Style)>,
        width: usize,
        now: i64,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let muted = Style::new().fg(theme.muted);
        let author = Style::new().fg(theme.fg).add_modifier(Modifier::BOLD);
        let mut left = vec![(self.author.clone(), author)];
        left.extend(said);
        let at = self.at.map(|at| (ago(at, now), muted));
        fitted(left, at, width)
    }

    /// Its reactions two columns apart, on as many rows as they take, with the reader's own in
    /// the primary color, as the desktop's reaction bar marks them out. No row where it has
    /// none, as the desktop hides an empty bar where the reader can't react, and t3term reacts
    /// to nothing.
    fn reacted(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let pills = self
            .reactions
            .iter()
            .map(|pill| {
                let color = if pill.mine {
                    theme.primary
                } else {
                    theme.muted
                };
                Span::styled(pill.text.clone(), Style::new().fg(color))
            })
            .collect();
        flowed(pills, "  ", width, theme)
    }

    /// The file and line it is on, and whether that line is still in the diff.
    fn place(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        match self.location() {
            Some(text) => whole(&text, width, Style::new().fg(theme.muted)),
            None => Vec::new(),
        }
    }

    /// `src/tui/mod.rs:42 · Outdated`, or None where it is on no file.
    fn location(&self) -> Option<String> {
        let location = self.location.as_ref()?;
        Some(match self.outdated {
            true => format!("{location} · Outdated"),
            false => location.clone(),
        })
    }

    /// Its text as laid out at `width`, and a note where T3 cut it short.
    fn text(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let muted = Style::new().fg(theme.muted);
        let mut lines = Vec::new();
        if let Some((at, laid_out)) = &self.laid_out
            && *at == width
        {
            lines.extend(laid_out.iter().cloned());
        }
        if self.body_cut {
            let note = format!(
                "Cut short: T3 sent a comment longer than the {} bytes t3term keeps.",
                grouped(pull_request::BODY as u64)
            );
            lines.extend(whole(&note, width, muted));
        }
        lines
    }
}

/// `reviewState` in words: lowercased, its underscores spaces, its first letter a capital.
fn state_label(state: &str) -> String {
    let words = state.to_lowercase().replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Each reviewer's last verdict, as `latestPullRequestReviewOutcomes` finds them: the newest
/// for each login, whatever order the host sent them in, with dismissed ones left out. Two
/// reviews with no author are two reviewers. A verdict given before the newest commit is
/// stale. A time that doesn't parse can't be said to come before or after another.
fn verdicts(comments: &[Comment], commits: &[pull_request::Commit]) -> Vec<Verdict> {
    let newest = commits
        .iter()
        .filter_map(|commit| instant(&commit.committed_at))
        .max();
    // In the order each login first gave a verdict, as the desktop's map keeps them.
    let mut latest: Vec<(Option<i64>, Verdict)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for comment in comments {
        let Some(outcome) = comment.review_state.as_deref().and_then(Outcome::of) else {
            continue;
        };
        let at = instant(&comment.created_at);
        let verdict = Verdict {
            reviewer: comment.author.as_ref().map(Reviewer::of),
            outcome,
            stale: matches!((at, newest), (Some(at), Some(newest)) if at < newest),
        };
        // Keyed by the lowercased login, as the reviewers are matched, where the desktop keys
        // by the login as sent. Otherwise a dismissal sent as `Octocat` would leave an
        // approval sent as `octocat` standing.
        let Some(key) = verdict
            .reviewer
            .as_ref()
            .map(|reviewer| reviewer.key.clone())
        else {
            latest.push((at, verdict));
            continue;
        };
        match index.get(&key) {
            Some(&at_index) => {
                let current = &mut latest[at_index];
                // Not every host sends its reviews in order, so the newest wins.
                if !matches!((current.0, at), (Some(was), Some(is)) if was > is) {
                    *current = (at, verdict);
                }
            }
            None => {
                index.insert(key, latest.len());
                latest.push((at, verdict));
            }
        }
    }
    latest
        .into_iter()
        .map(|(_, verdict)| verdict)
        .filter(|verdict| verdict.outcome != Outcome::Dismissed)
        .collect()
}

/// The Reviewers section's rows: each reviewer with their verdict at the end of their row,
/// then each who gave a verdict without being asked. The conversation's reviewers take the
/// detail's place where the host gave them.
pub(super) fn reviewer_lines(
    asked: &[Reviewer],
    conversation: Option<&Conversation>,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let value = Style::new().fg(theme.fg);
    let reviewers = conversation
        .and_then(|conversation| conversation.reviewers.as_deref())
        .unwrap_or(asked);
    let verdicts = conversation.map_or(&[][..], |conversation| &conversation.verdicts);
    let by_key: HashMap<&str, &Verdict> = verdicts
        .iter()
        .filter_map(|verdict| Some((verdict.reviewer.as_ref()?.key.as_str(), verdict)))
        .collect();
    let mut lines = Vec::new();
    for reviewer in reviewers {
        let verdict = by_key.get(reviewer.key.as_str());
        let right = verdict.map(|verdict| (verdict.label(), Style::new().fg(verdict.color(theme))));
        lines.extend(fitted(vec![(reviewer.label(), value)], right, width));
    }
    for verdict in verdicts {
        let listed = verdict
            .reviewer
            .as_ref()
            .is_some_and(|reviewer| reviewers.iter().any(|asked| asked.key == reviewer.key));
        if listed {
            continue;
        }
        let label = verdict
            .reviewer
            .as_ref()
            .map_or_else(|| "ghost".to_string(), Reviewer::label);
        let right = (verdict.label(), Style::new().fg(verdict.color(theme)));
        lines.extend(fitted(vec![(label, value)], Some(right), width));
    }
    if lines.is_empty() {
        lines = whole("None", width, muted);
    }
    lines
}

/// The rows of each control among some lines, in the order they show.
pub(super) type Targets = Vec<(Range<usize>, Control)>;

/// The Comments section: its heading, then its rows as the conversation's read stands, and
/// the rows of each control among them, counted from the heading's.
pub(super) struct Section {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) targets: Targets,
}

/// Builds a section's rows: items with a blank row between each two, and the rows of each
/// control.
struct Rows {
    lines: Vec<Line<'static>>,
    targets: Targets,
    /// Where the first item goes, under the heading.
    first: usize,
}

impl Rows {
    /// Adds an item, the first `active` of whose rows do what `control` does.
    fn item(&mut self, lines: Vec<Line<'static>>, control: Option<(Control, usize)>) {
        if lines.is_empty() {
            return;
        }
        if self.lines.len() > self.first {
            self.lines.push(Line::default());
        }
        let start = self.lines.len();
        if let Some((control, active)) = control {
            self.targets
                .push((start..start + active.min(lines.len()), control));
        }
        self.lines.extend(lines);
    }

    /// Adds an item whose every row does what `control` does.
    fn control(&mut self, lines: Vec<Line<'static>>, control: Control) {
        let rows = lines.len();
        self.item(lines, Some((control, rows)));
    }
}

/// The Comments section as the conversation's read stands and the reader opened it. Its
/// heading holds the switch between newest and oldest first once there are remarks.
pub(super) fn section(
    answer: &Answer<Conversation>,
    shown: &Shown,
    width: usize,
    now: i64,
    theme: &Theme,
) -> Section {
    let muted = Style::new().fg(theme.muted);
    let value = Style::new().fg(theme.fg);
    let heading = |label: &str| vec![Line::styled(clip(label, width), muted)];
    let conversation = match answer {
        Answer::Reading => {
            let mut lines = heading("Comments");
            lines.extend(whole("Reading the conversation…", width, muted));
            return Section {
                lines,
                targets: Vec::new(),
            };
        }
        Answer::Failed(failure) => {
            let mut lines = heading("Comments");
            lines.extend(whole("Could not load pull request activity", width, value));
            lines.extend(whole(failure, width, muted));
            return Section {
                lines,
                targets: Vec::new(),
            };
        }
        Answer::Read(conversation) => conversation,
    };
    let label = format!("Comments ({})", grouped(conversation.count));
    let mut rows = Rows {
        lines: heading(&label),
        targets: Vec::new(),
        first: 0,
    };
    if conversation.has(Control::Order, shown) && width > 0 {
        let order = match shown.oldest_first {
            true => "Oldest first",
            false => "Newest first",
        };
        if label.width() + 1 + order.width() <= width {
            let left = vec![Span::styled(label.clone(), muted)];
            rows.lines = vec![row(left, vec![Span::styled(order, muted)], width)];
            rows.targets.push((0..1, Control::Order));
        } else {
            let at = rows.lines.len();
            rows.lines.extend(whole(order, width, muted));
            rows.targets.push((at..rows.lines.len(), Control::Order));
        }
    }
    rows.first = rows.lines.len();
    if conversation.truncated {
        let note = format!(
            "This conversation is longer than this page reads in one go. The most recent {} \
             are here; open it on the host to read the rest.",
            grouped(conversation.sent as u64)
        );
        rows.item(whole(&note, width, Style::new().fg(theme.warning_fg)), None);
    }
    if conversation.sent == 0 {
        rows.item(whole("No comments yet.", width, muted), None);
    }
    let in_order = |remarks: &'_ [Remark]| -> Vec<usize> {
        let mut places: Vec<usize> = (0..remarks.len()).collect();
        if !shown.oldest_first {
            places.reverse();
        }
        places
    };

    // The 10 most recent active remarks, with Show older on the side the older ones are on.
    let active = &conversation.active;
    let from = active.len().saturating_sub(shown.active);
    let older = button("", from, "comment", width, theme);
    if shown.oldest_first && conversation.has(Control::Older, shown) {
        rows.control(older.clone(), Control::Older);
    }
    for at in in_order(&active[from..]) {
        rows.item(active[from + at].lines(width, now, theme), None);
    }
    if !shown.oldest_first && conversation.has(Control::Older, shown) {
        rows.control(older, Control::Older);
    }
    if conversation.has(Control::Recent, shown) {
        let text = format!("Show only {COMMENT_PAGE} recent comments");
        rows.control(whole(&text, width, muted), Control::Recent);
    }

    // The bots' remarks, as a group that opens to its 10 most recent.
    if conversation.has(Control::Bots, shown) {
        let group = group_lines(
            &conversation.bots_group,
            conversation.bots.len(),
            "bot comment",
            shown.bots_open,
            width,
            now,
            theme,
        );
        rows.control(group, Control::Bots);
    }
    if shown.bots_open {
        let bots = &conversation.bots;
        let from = bots.len().saturating_sub(shown.bots);
        for at in in_order(&bots[from..]) {
            rows.item(bots[from + at].lines(width, now, theme), None);
        }
        if conversation.has(Control::OlderBots, shown) {
            let older = button("bot ", from, "comment", width, theme);
            rows.control(older, Control::OlderBots);
        }
        if conversation.has(Control::RecentBots, shown) {
            let text = format!("Show only {COMMENT_PAGE} recent bot comments");
            rows.control(whole(&text, width, muted), Control::RecentBots);
        }
    }

    // The resolved and dismissed remarks, as a group whose remarks each open.
    if conversation.has(Control::Finished, shown) {
        let group = group_lines(
            &conversation.finished_group,
            conversation.finished.len(),
            "resolved or dismissed comment",
            shown.finished_open,
            width,
            now,
            theme,
        );
        rows.control(group, Control::Finished);
    }
    if shown.finished_open {
        for at in in_order(&conversation.finished) {
            let remark = &conversation.finished[at];
            let open = shown.opened.contains(&remark.id);
            let (lines, active) = remark.finished_lines(open, width, now, theme);
            rows.item(lines, Some((Control::Remark(at), active)));
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

/// `+ Show 10 older comments (12 hidden)`, of the `hidden` remarks older than those that show.
fn button(
    kind: &str,
    hidden: usize,
    what: &str,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let plural = if hidden == 1 { "" } else { "s" };
    let text = format!(
        "+ Show {} older {kind}{what}{plural} ({} hidden)",
        grouped(hidden.min(COMMENT_PAGE) as u64),
        grouped(hidden as u64)
    );
    whole(&text, width, Style::new().fg(theme.muted))
}

/// A group's rows: a chevron and `2 bot comments`, then who wrote them, on how many files and
/// when the latest came.
fn group_lines(
    group: &Group,
    count: usize,
    what: &str,
    open: bool,
    width: usize,
    now: i64,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let chevron = if open { "⌄" } else { "›" };
    let plural = if count == 1 { "" } else { "s" };
    let label = format!("{chevron} {} {what}{plural}", grouped(count as u64));
    let mut lines = whole(&label, width, Style::new().fg(theme.fg));
    lines.extend(whole(&group.line(now), width, Style::new().fg(theme.muted)));
    lines
}

/// `left`'s pieces two columns apart with `right` at the end of the row, when they fit in
/// `width`. Otherwise the first piece takes rows of its own, wrapped to every row it takes, and
/// the rest fit the same way on the rows under it.
fn fitted(
    mut left: Vec<(String, Style)>,
    right: Option<(String, Style)>,
    width: usize,
) -> Vec<Line<'static>> {
    let gaps = 2 * left.len().saturating_sub(1);
    let used: usize = left.iter().map(|(text, _)| text.width()).sum::<usize>() + gaps;
    let room = right.as_ref().map_or(0, |(text, _)| text.width() + 1);
    if used + room <= width {
        let mut spans = Vec::new();
        for (at, (text, style)) in left.into_iter().enumerate() {
            if at > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(text, style));
        }
        return match right {
            Some((text, style)) => vec![row(spans, vec![Span::styled(text, style)], width)],
            None => vec![Line::from(spans)],
        };
    }
    if left.is_empty() {
        return right.map_or_else(Vec::new, |(text, style)| whole(&text, width, style));
    }
    let (text, style) = left.remove(0);
    let mut lines = whole(&text, width, style);
    lines.extend(fitted(left, right, width));
    lines
}

/// How long before `now` the moment `then` was, as the desktop's `formatRelativeTimeLabel`
/// writes it: `just now`, `5m ago`, `3h ago` or `2d ago`.
fn ago(then: i64, now: i64) -> String {
    match age(then, now).as_str() {
        "now" => "just now".to_string(),
        ago => format!("{ago} ago"),
    }
}

/// Milliseconds since the epoch for a host's ISO-8601 time, its offset applied, as the
/// desktop's `Date.parse` reads one. Every host sends ISO-8601, but not all in UTC, so
/// `2026-07-05T01:00:00+02:00` comes before `2026-07-05T00:30:00Z`. A time without an offset
/// reads as UTC. None for anything else.
fn instant(text: &str) -> Option<i64> {
    let base = parse_iso_ms(text)?;
    let rest = text.get(19..)?;
    let rest = match rest.strip_prefix('.') {
        Some(fraction) => fraction.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => rest,
    };
    let (sign, offset) = match rest.as_bytes().first() {
        None => return Some(base),
        Some(b'Z' | b'z') if rest.len() == 1 => return Some(base),
        Some(b'+') => (1, &rest[1..]),
        Some(b'-') => (-1, &rest[1..]),
        _ => return None,
    };
    if !offset.bytes().all(|b| b.is_ascii_digit() || b == b':') {
        return None;
    }
    let (hours, minutes) = match (offset.len(), offset.as_bytes().get(2)) {
        (5, Some(b':')) => (&offset[..2], &offset[3..]),
        (4, _) => (&offset[..2], &offset[2..]),
        (2, _) => (offset, "00"),
        _ => return None,
    };
    let (hours, minutes): (i64, i64) = (hours.parse().ok()?, minutes.parse().ok()?);
    Some(base - sign * (hours * 60 + minutes) * 60_000)
}

/// A finished remark's text on one row, as the desktop's `CollapsedComment` writes it: without
/// HTML comments, even in code, or an alert's `[!NOTE]` row, with each link and image as its
/// text, without the marks that start a quote, heading or list item or any `*` or backtick,
/// and each run of whitespace one space. Each step takes time in proportion to the text, as
/// the remark can be a quarter of a megabyte.
fn preview(text: &str) -> String {
    let text = uncommented(text);
    let kept: Vec<&str> = text
        .split('\n')
        .map(|line| if is_alert(line) { "" } else { line })
        .collect();
    let text = unlinked(&kept.join("\n"));
    let mut out = String::new();
    for line in text.split('\n') {
        let line = line.trim_start_matches(|c: char| c.is_whitespace() || ">#*-".contains(c));
        for c in line.chars() {
            match c {
                '*' | '`' => {}
                c if c.is_whitespace() => {
                    if !out.ends_with(' ') {
                        out.push(' ');
                    }
                }
                c => out.push(c),
            }
        }
        if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

/// `text` without anything from `<!--` to the next `-->`. One never closed stays.
fn uncommented(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("<!--") {
        let Some(close) = rest[open + 4..].find("-->") else {
            break;
        };
        out.push_str(&rest[..open]);
        rest = &rest[open + 4 + close + 3..];
    }
    out.push_str(rest);
    out
}

/// Whether `line` is an alert's marker row, such as `> [!NOTE]`.
fn is_alert(line: &str) -> bool {
    let line = line.trim();
    let line = line.strip_prefix('>').unwrap_or(line).trim();
    line.strip_prefix("[!")
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|kind| {
            !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// `text` with each `[text](url)` and `![text](url)` as its text, as the desktop's
/// `!?(\[([^\]]+)\])\([^)]*\)` finds them. A link's text and target may span rows.
fn unlinked(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        // Every `[` before the first `]` closes there, so where one fails they all do.
        let Some(close) = after.find(']') else {
            break;
        };
        let target = after[close + 1..].strip_prefix('(');
        let end = target.map(|target| target.find(')'));
        match (target, end) {
            (Some(target), Some(Some(end))) if close > 0 => {
                let before = &rest[..open];
                out.push_str(before.strip_suffix('!').unwrap_or(before));
                out.push_str(&after[..close]);
                rest = &target[end + 1..];
            }
            // With no `)` left, no link is.
            (Some(_), Some(None)) => break,
            _ => {
                out.push_str(&rest[..open + 1 + close + 1]);
                rest = &after[close + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `text` without its HTML comments, which review bots keep their bookkeeping in and the
/// desktop's Markdown renderer drops (rehype-sanitize in ChatMarkdown.tsx). A comment inside
/// code stays, since it is part of the code shown: a fenced code block, or a code span, which
/// CommonMark reads before a comment when its backticks come first.
fn without_html_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<(char, usize)> = None;
    let mut in_comment = false;
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if in_comment {
            let Some(end) = rest.find("-->") else {
                break;
            };
            at += end + 3;
            in_comment = false;
            continue;
        }
        if at == 0 || text.as_bytes()[at - 1] == b'\n' {
            let line = rest.split_inclusive('\n').next().unwrap_or(rest);
            let ends = |(mark, length, after): (char, usize, &str)| {
                fence.is_some_and(|(open, opened)| {
                    mark == open && length >= opened && after.trim().is_empty()
                })
            };
            let inside = match fence {
                None => {
                    fence = opens_fence(line);
                    fence.is_some()
                }
                // Only its own mark, at least as long and with nothing after it, closes a fence.
                Some(_) => {
                    if fence_of(line).is_some_and(ends) {
                        fence = None;
                    }
                    true
                }
            };
            if inside {
                out.push_str(line);
                at += line.len();
                continue;
            }
        }
        let mut chars = rest.chars();
        let first = chars.next().unwrap_or_default();
        let taken = match first {
            // An escaped backtick or bracket opens nothing.
            '\\' => chars
                .next()
                .filter(char::is_ascii_punctuation)
                .map_or(1, |escaped| 1 + escaped.len_utf8()),
            '`' => {
                let run = rest.len() - rest.trim_start_matches('`').len();
                code_span(rest, run).unwrap_or(run)
            }
            '<' if rest.starts_with("<!--") => {
                in_comment = true;
                at += 4;
                continue;
            }
            other => other.len_utf8(),
        };
        out.push_str(&rest[..taken]);
        at += taken;
    }
    out
}

/// The length of the code span `text` starts with, through the run of exactly `run` backticks
/// that closes it later in the same paragraph, as CommonMark reads one. None when nothing
/// closes it, so its backticks are text. A blank line or a fence ends the paragraph.
fn code_span(text: &str, run: usize) -> Option<usize> {
    let mut at = run;
    for (index, line) in text[run..].split_inclusive('\n').enumerate() {
        if index > 0 && (line.trim().is_empty() || opens_fence(line).is_some()) {
            return None;
        }
        let bytes = line.as_bytes();
        let mut end = 0;
        while end < bytes.len() {
            if bytes[end] != b'`' {
                end += 1;
                continue;
            }
            let start = end;
            while bytes.get(end) == Some(&b'`') {
                end += 1;
            }
            if end - start == run {
                return Some(at + end);
            }
        }
        at += line.len();
    }
    None
}

/// The mark and length of the code fence `line` opens, if it opens one.
fn opens_fence(line: &str) -> Option<(char, usize)> {
    let (mark, length, info) = fence_of(line)?;
    // A backtick fence's info string can't hold a backtick, or it is inline code.
    (mark == '~' || !info.contains('`')).then_some((mark, length))
}

/// The mark, the length and the rest of a code fence `line` starts with: three or more of the
/// same backtick or tilde, as CommonMark reads one.
fn fence_of(line: &str) -> Option<(char, usize, &str)> {
    let line = line.trim_start();
    let mark = line
        .chars()
        .next()
        .filter(|mark| matches!(mark, '`' | '~'))?;
    let rest = line.trim_start_matches(mark);
    let length = line.len() - rest.len();
    (length >= 3).then_some((mark, length, rest))
}

#[cfg(test)]
mod tests;
