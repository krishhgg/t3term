//! A review thread as the Code tab shows it, under the line it is on or in the list of those
//! off the diff, as the desktop's `ReviewThreadCard` does
//! (apps/web/src/components/pullRequest/PullRequestReviewAnnotation.tsx at f570bd2).

use std::collections::HashSet;
use std::ops::Range;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::super::theme::Theme;
use super::super::{flowed, grouped, whole};
use super::{More, Remark, Thread};
use crate::pull_request::{CommentPage, MOST_THREAD_COMMENTS, ReviewThread};

/// The columns the bar down a thread's left takes.
const BAR: usize = 2;
const LOADING: &str = "Loading more comments…";
const FAILED: &str = "t3term couldn't read more comments.";
const LOAD: &str = "Load more comments";

/// The width a thread's remarks are laid out at in `width`, beside the bar.
pub(super) fn inner(width: usize) -> usize {
    if width > BAR { width - BAR } else { width }
}

/// A thread's rows, with the ones Enter or a click acts on.
pub(in super::super) struct Card {
    pub(in super::super) lines: Vec<Line<'static>>,
    /// How many of the rows open and close it: those of the first, however many it wraps to.
    pub(in super::super) header: usize,
    /// The rows that read its next page of remarks, where they show.
    pub(in super::super) more: Option<Range<usize>>,
}

impl Thread {
    pub(super) fn of(thread: &ReviewThread) -> Thread {
        let remarks: Vec<Remark> = thread.comments.iter().map(Remark::of_thread).collect();
        // T3 sent as many remarks as t3term keeps, and perhaps more it left out, which a page
        // read after them would skip.
        let whole = remarks.len() >= MOST_THREAD_COMMENTS;
        Thread {
            id: thread.id.clone(),
            path: thread.path.clone(),
            line: thread.line,
            side: thread.side,
            resolved: thread.resolved,
            outdated: thread.outdated,
            count: thread.comment_count.unwrap_or(thread.comments.len() as u64),
            remarks,
            loaded: Vec::new(),
            next: thread.next_comments.clone().filter(|_| !whole),
            paged: false,
            more: More::Idle,
        }
    }

    /// Whether Enter reads its next page: there is one, and no read of it is under way.
    pub(in super::super) fn offers_more(&self) -> bool {
        self.next.is_some() && !matches!(self.more, More::Wanted | More::Asked(_))
    }

    /// Takes `page`, the page of remarks after those it holds: each remark it doesn't have
    /// yet, up to as many as t3term keeps, and where the page after starts, as the desktop's
    /// `mergePullRequestThreadComments` keeps the first of each id
    /// (apps/web/src/components/pullRequest/pullRequestDetail.logic.ts:170-184).
    pub(super) fn take(&mut self, page: &CommentPage) {
        let mut seen: HashSet<String> = self.ids().map(str::to_string).collect();
        for comment in &page.comments {
            if self.remarks.len() + self.loaded.len() >= MOST_THREAD_COMMENTS {
                break;
            }
            if seen.insert(comment.id.clone()) {
                self.loaded.push(Remark::of_thread(comment));
            }
        }
        let full = self.remarks.len() + self.loaded.len() >= MOST_THREAD_COMMENTS;
        self.next = page.next_cursor.clone().filter(|_| !page.cut && !full);
        self.paged = true;
        self.more = More::Idle;
    }

    /// Takes from `old`, the same thread as the conversation read before had it, the remarks
    /// its pages read that this doesn't have, and where the next page and its read are. Where
    /// its pages ran out and the host now counts more remarks than it holds, as after a reply,
    /// this read's own cursor reads on, and the remarks it holds are skipped, as the desktop
    /// reopens the cursor after a reply (PullRequestReviewAnnotation.tsx:175-182).
    pub(super) fn carry(&mut self, old: &mut Thread) {
        if old.paged {
            let have: HashSet<&str> = self.remarks.iter().map(|r| r.id.as_str()).collect();
            let room = MOST_THREAD_COMMENTS.saturating_sub(self.remarks.len());
            let loaded: Vec<Remark> = std::mem::take(&mut old.loaded)
                .into_iter()
                .filter(|remark| !have.contains(remark.id.as_str()))
                .take(room)
                .collect();
            let held = self.remarks.len() + loaded.len();
            let full = held >= MOST_THREAD_COMMENTS;
            self.loaded = loaded;
            let fresh = self.next.take().filter(|_| self.count > held as u64);
            self.next = old.next.take().or(fresh).filter(|_| !full);
            self.paged = true;
        }
        self.more = std::mem::take(&mut old.more);
    }

    /// The ids of the remarks it holds.
    fn ids(&self) -> impl Iterator<Item = &str> {
        self.remarks
            .iter()
            .chain(&self.loaded)
            .map(|remark| remark.id.as_str())
    }

    /// Its rows at `width`, behind a bar down the left: a row that says whether it is open or
    /// resolved, how many remarks the host counts and whether its line is outdated, then while
    /// it is `open` each remark, and `Load more comments` where the host has another page, or
    /// how many more the host has.
    pub(in super::super) fn lines(
        &self,
        open: bool,
        width: usize,
        now: i64,
        theme: &Theme,
    ) -> Card {
        let inner = inner(width);
        let muted = Style::new().fg(theme.muted);
        let chevron = if open { "⌄" } else { "›" };
        let state = match self.resolved {
            true => Span::styled(
                format!("{chevron} Resolved"),
                Style::new().fg(theme.success),
            ),
            false => Span::styled(format!("{chevron} Open"), Style::new().fg(theme.fg)),
        };
        let noun = if self.count == 1 {
            "comment"
        } else {
            "comments"
        };
        let mut parts = vec![
            state,
            Span::styled(format!("{} {noun}", grouped(self.count)), muted),
        ];
        if self.outdated {
            parts.push(Span::styled("outdated", muted));
        }
        let mut lines = flowed(parts, " · ", inner, theme);
        let header = lines.len();
        let mut more = None;
        if open {
            for (at, remark) in self.remarks.iter().chain(&self.loaded).enumerate() {
                if at > 0 {
                    lines.push(Line::default());
                }
                lines.extend(remark.lines(inner, now, theme));
            }
            match (&self.more, &self.next) {
                (More::Wanted | More::Asked(_), _) => lines.extend(whole(LOADING, inner, muted)),
                (failed, Some(_)) => {
                    if let More::Failed(failure) = failed {
                        lines.extend(whole(FAILED, inner, muted));
                        lines.extend(whole(failure, inner, muted));
                    }
                    let start = lines.len();
                    lines.extend(whole(LOAD, inner, Style::new().fg(theme.fg)));
                    more = Some(start..lines.len());
                }
                (_, None) => {
                    let held = (self.remarks.len() + self.loaded.len()) as u64;
                    let left = self.count.saturating_sub(held);
                    if left > 0 {
                        let noun = if left == 1 { "comment" } else { "comments" };
                        let note = format!("{} more {noun} on the host.", grouped(left));
                        lines.extend(whole(&note, inner, muted));
                    }
                }
            }
        }
        if inner < width {
            let bar = Style::new().fg(theme.border_strong);
            for line in &mut lines {
                line.spans.insert(0, Span::styled("│ ", bar));
            }
        }
        Card {
            lines,
            header,
            more,
        }
    }
}
