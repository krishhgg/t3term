//! A pull request's conversation in a link's details, from T3's `pullRequests.activity`, as the
//! desktop's Summary tab shows it under Comments and beside each reviewer
//! (apps/web/src/components/pullRequest/PullRequestSummaryTab.tsx and pullRequestDetail.logic.ts
//! at f570bd2).
//!
//! The remarks split as the desktop splits them. One in a resolved review thread, or a
//! dismissed review, is finished. Otherwise one whose author is a bot, or whose login ends in
//! `[bot]`, is a bot's. The rest are active, and the 10 most recent of those show, newest
//! first. The others are counted. Each reviewer shows their last verdict, read from the whole
//! conversation, and a verdict older than the newest commit reads as given on earlier changes.
//!
//! What shows is cleaned and bounded once, when the answer arrives. Each remark's Markdown is
//! laid out once for each width.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::super::markdown::{self, Styles};
use super::super::row;
use super::super::setup::bounded;
use super::super::theme::{Theme, age, parse_iso_ms};
use super::{Answer, NAME, grouped, whole};
use crate::pull_request::{self, Actor, Comment, Commit, ReviewThread};
use crate::transcript::without_controls;

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

/// One remark that shows, cleaned and bounded.
struct Remark {
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
    /// The text laid out at one width.
    laid_out: Option<(usize, Vec<Line<'static>>)>,
}

/// What the details show of a pull request's conversation.
pub(super) struct Conversation {
    /// How many remarks the host counts, which the section's label shows.
    count: u64,
    /// T3 read only the most recent remarks.
    truncated: bool,
    /// How many remarks T3 sent.
    sent: usize,
    /// The most recent active remarks, newest first.
    recent: Vec<Remark>,
    /// Active remarks older than those.
    older: usize,
    bots: usize,
    finished: usize,
    /// The reviewers, where the host gave them with the conversation. They take the place of
    /// the detail's.
    reviewers: Option<Vec<Reviewer>>,
    verdicts: Vec<Verdict>,
    /// T3 sent more remarks, review threads, commits or reviewers than t3term keeps.
    cut: bool,
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
        let mut active = Vec::new();
        let (mut bots, mut finished) = (0, 0);
        for comment in &activity.comments {
            let thread = threads.get(comment.id.as_str()).copied();
            let outcome = comment.review_state.as_deref().and_then(Outcome::of);
            let author = comment.author.as_ref();
            if thread.is_some_and(|thread| thread.resolved) || outcome == Some(Outcome::Dismissed) {
                finished += 1;
            } else if author.is_some_and(|actor| actor.is_bot || actor.login.ends_with("[bot]")) {
                bots += 1;
            } else {
                active.push((comment, thread));
            }
        }
        // The window is the most recent remarks in the order T3 sent them, shown newest first.
        let older = active.len().saturating_sub(COMMENT_PAGE);
        let recent = active[older..]
            .iter()
            .rev()
            .map(|(comment, thread)| Remark::of(comment, *thread))
            .collect();
        let reviewers = activity
            .reviewers
            .as_ref()
            .map(|reviewers| reviewers.iter().map(Reviewer::of).collect());
        Conversation {
            count: activity.comment_count,
            truncated: activity.comments_truncated,
            sent: activity.comments.len(),
            recent,
            older,
            bots,
            finished,
            reviewers,
            verdicts: verdicts(&activity.comments, &activity.commits),
            cut: activity.cut,
        }
    }

    /// Lays each remark's text out at `width`, unless it already is.
    pub(super) fn lay_out(&mut self, width: usize, theme: &Theme) {
        for remark in &mut self.recent {
            remark.lay_out(width, theme);
        }
    }
}

impl Remark {
    fn of(comment: &Comment, thread: Option<&ReviewThread>) -> Remark {
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
        let text = without_controls(&without_html_comments(&comment.body.text));
        let text = text.trim();
        Remark {
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

    fn lines(&self, width: usize, now: i64, theme: &Theme) -> Vec<Line<'static>> {
        let muted = Style::new().fg(theme.muted);
        let author = Style::new().fg(theme.fg).add_modifier(Modifier::BOLD);
        let mut left = vec![(self.author.clone(), author)];
        match &self.said {
            Some(Said::Verdict(outcome)) => {
                left.push((
                    outcome.label().to_string(),
                    Style::new().fg(outcome.color(theme)),
                ));
            }
            Some(Said::State(state)) => left.push((state.clone(), muted)),
            None => {}
        }
        let at = self.at.map(|at| (ago(at, now), muted));
        let mut lines = fitted(left, at, width);
        if let Some(location) = &self.location {
            let text = match self.outdated {
                true => format!("{location} · Outdated"),
                false => location.clone(),
            };
            lines.extend(whole(&text, width, muted));
        }
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
fn verdicts(comments: &[Comment], commits: &[Commit]) -> Vec<Verdict> {
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

/// The Comments section's label and rows, as the conversation's read stands.
pub(super) fn lines(
    answer: &Answer<Conversation>,
    width: usize,
    now: i64,
    theme: &Theme,
) -> (String, Vec<Line<'static>>) {
    let muted = Style::new().fg(theme.muted);
    let value = Style::new().fg(theme.fg);
    let conversation = match answer {
        Answer::Reading => {
            let rows = whole("Reading the conversation…", width, muted);
            return ("Comments".to_string(), rows);
        }
        Answer::Failed(failure) => {
            let mut rows = whole("Could not load pull request activity", width, value);
            rows.extend(whole(failure, width, muted));
            return ("Comments".to_string(), rows);
        }
        Answer::Read(conversation) => conversation,
    };
    let mut rows = Vec::new();
    if conversation.truncated {
        let note = format!(
            "This conversation is longer than this page reads in one go. The most recent {} \
             are here; open it on the host to read the rest.",
            grouped(conversation.sent as u64)
        );
        rows.extend(whole(&note, width, Style::new().fg(theme.warning_fg)));
    }
    if conversation.sent == 0 {
        rows.extend(whole("No comments yet.", width, muted));
    }
    for (at, remark) in conversation.recent.iter().enumerate() {
        if at > 0 {
            rows.push(Line::default());
        }
        rows.extend(remark.lines(width, now, theme));
    }
    let counts = [
        (conversation.older, "older comment"),
        (conversation.bots, "bot comment"),
        (conversation.finished, "resolved or dismissed comment"),
    ];
    let mut first = true;
    for (count, what) in counts {
        if count == 0 {
            continue;
        }
        if first && !conversation.recent.is_empty() {
            rows.push(Line::default());
        }
        first = false;
        let plural = if count == 1 { "" } else { "s" };
        let text = format!("{} {what}{plural} not shown.", grouped(count as u64));
        rows.extend(whole(&text, width, muted));
    }
    if conversation.cut {
        let note = "T3 sent more comments, review threads, commits or reviewers than t3term keeps.";
        rows.extend(whole(note, width, muted));
    }
    let label = format!("Comments ({})", grouped(conversation.count));
    (label, rows)
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

/// `text` without its HTML comments, which review bots keep their bookkeeping in and the
/// desktop's Markdown renderer drops. A comment inside a fenced code block stays, since it is
/// part of the code shown.
fn without_html_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<(char, usize)> = None;
    let mut in_comment = false;
    for line in text.split_inclusive('\n') {
        if !in_comment {
            match (fence, fence_of(line)) {
                // A backtick fence's info string can't hold a backtick, or it is inline code.
                (None, Some((mark, length, info))) if mark == '~' || !info.contains('`') => {
                    fence = Some((mark, length));
                    out.push_str(line);
                    continue;
                }
                // Only its own mark, at least as long and with nothing after it, closes a fence.
                (Some((open, opened)), Some((mark, length, rest)))
                    if mark == open && length >= opened && rest.trim().is_empty() =>
                {
                    fence = None;
                    out.push_str(line);
                    continue;
                }
                (Some(_), _) => {
                    out.push_str(line);
                    continue;
                }
                _ => {}
            }
        }
        let mut rest = line;
        loop {
            if in_comment {
                match rest.find("-->") {
                    Some(end) => {
                        rest = &rest[end + 3..];
                        in_comment = false;
                    }
                    None => break,
                }
            } else {
                match rest.find("<!--") {
                    Some(start) => {
                        out.push_str(&rest[..start]);
                        rest = &rest[start + 4..];
                        in_comment = true;
                    }
                    None => {
                        out.push_str(rest);
                        break;
                    }
                }
            }
        }
    }
    out
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
