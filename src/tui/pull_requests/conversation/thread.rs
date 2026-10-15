//! A review thread as the Code tab shows it, under the line it is on or in the list of those
//! off the diff, as the desktop's `ReviewThreadCard` does
//! (apps/web/src/components/pullRequest/PullRequestReviewAnnotation.tsx at f570bd2).

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::super::theme::Theme;
use super::super::{flowed, grouped};
use super::{Remark, Thread};
use crate::pull_request::ReviewThread;

/// The columns the bar down a thread's left takes.
const BAR: usize = 2;

/// The width a thread's remarks are laid out at in `width`, beside the bar.
pub(super) fn inner(width: usize) -> usize {
    if width > BAR { width - BAR } else { width }
}

impl Thread {
    pub(super) fn of(thread: &ReviewThread) -> Thread {
        Thread {
            id: thread.id.clone(),
            path: thread.path.clone(),
            line: thread.line,
            side: thread.side,
            resolved: thread.resolved,
            outdated: thread.outdated,
            count: thread.comment_count.unwrap_or(thread.comments.len() as u64),
            remarks: thread.comments.iter().map(Remark::of_thread).collect(),
        }
    }

    /// Its rows at `width`, behind a bar down the left: a row that says whether it is open or
    /// resolved, how many remarks the host counts and whether its line is outdated, then while
    /// it is `open` each remark and how many more the host has. The second number is how
    /// many of the rows open and close it: those of the first row, however many it wraps to.
    pub(in super::super) fn lines(
        &self,
        open: bool,
        width: usize,
        now: i64,
        theme: &Theme,
    ) -> (Vec<Line<'static>>, usize) {
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
        if open {
            for (at, remark) in self.remarks.iter().enumerate() {
                if at > 0 {
                    lines.push(Line::default());
                }
                lines.extend(remark.lines(inner, now, theme));
            }
            let more = self.count.saturating_sub(self.remarks.len() as u64);
            if more > 0 {
                let noun = if more == 1 { "comment" } else { "comments" };
                let note = format!("{} more {noun} on the host.", grouped(more));
                lines.extend(super::super::whole(&note, inner, muted));
            }
        }
        if inner < width {
            let bar = Style::new().fg(theme.border_strong);
            for line in &mut lines {
                line.spans.insert(0, Span::styled("│ ", bar));
            }
        }
        (lines, header)
    }
}
