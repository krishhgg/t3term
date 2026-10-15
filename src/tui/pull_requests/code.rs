//! The Code tab of a link's details: the pull request's diff, as the desktop's
//! `PullRequestCodeTab` shows it (apps/web/src/components/pullRequest/PullRequestCodeTab.tsx at
//! f570bd2).
//!
//! The tab reads the diff through T3's `/api/pull-requests/diff` once it first shows, a slice at
//! a time, and reads the next slice once the end of the files comes within a page of the view,
//! as the desktop's sentinel does 240 pixels ahead. It reads the diff again from its first slice
//! when it shows after `FRESH` or more, after a read of it failed, and after `r`, a new refresh
//! number from T3 or a new `updatedAt` on the detail, as the desktop's refresh token says
//! (PullRequestDetailPanel.tsx:832-847). The slices read show until the new first one comes.
//!
//! Each slice's files show in the desktop's reading order, source before tests before the files
//! a tool wrote, as `order` says. Each file shows folded on one row, as the desktop's do by
//! default: whether the change added,
//! deleted, renamed or modified it, its path, and the lines it adds and removes. Enter or a
//! click unfolds it into its hunks. Each line has one number, the one it has after the change,
//! or before it for a removed line, as the desktop's stacked view has, and the unchanged lines
//! between two hunks are counted on a row of their own.
//!
//! Only the files unfolded are laid out. Each line of them is cleaned of control characters,
//! its tabs set every `TAB` columns, and cut at `LINE` characters as it is.

use std::collections::{HashMap, HashSet};

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use super::super::clip;
use super::super::setup::bounded;
use super::super::theme::Theme;
use super::conversation::{Control, Targets};
use super::{FAILURE, FRESH, grouped, whole};
use crate::pull_request::{self, patch};
use crate::transcript::clusters;

/// The columns between two tab stops in a line of the diff.
const TAB: usize = 4;
/// The most characters of a line of the diff shown. A longer one, such as a line of a minified
/// file, is cut with `…`.
const LINE: usize = 2000;
/// The longest path shown, in characters.
const PATH: usize = 1024;

const LOADING: &str = "Loading pull request diff…";
const LOADING_MORE: &str = "Loading more files…";
const EMPTY: &str = "This pull request has no file changes.";
const WITHHELD: &str =
    "The host withheld part of this diff, such as a binary file or a change too large to show.";
const REST_FAILED: &str = "t3term couldn't read the rest of this diff.";
const STALE: &str = "May be out of date: t3term couldn't read the diff again.";
const UNREAD: &str = "t3term couldn't split this patch into files.";

/// What the Code tab read of a pull request's diff.
pub(super) struct Code {
    /// The slices read, in the order T3 sent them.
    slices: Vec<Slice>,
    /// The files of every slice, as a slice's place and a file's place in it: the slices in the
    /// order T3 sent them, and each slice's files in reading order. A path a slice has twice
    /// shows once, as its last part, as the desktop's `orderDiffFiles` keeps it.
    files: Vec<(usize, usize)>,
    /// The lines the host counted for each file whose changes it withheld, by path, from every
    /// slice.
    omitted: HashMap<String, (u64, u64)>,
    /// The read under way.
    asked: Option<Asked>,
    /// Why the last read failed, cleaned and bounded, and whether it read the first slice.
    failed: Option<(String, bool)>,
    /// When the first slice shown came, which `FRESH` counts from.
    at: i64,
    /// Set when the diff is to be read from its first slice: before the first read, and once
    /// it may be out of date. A read under way then is dropped when it comes.
    again: bool,
    /// Set when the end of the files came within a page of the view, so the next slice is read.
    due: bool,
    /// The paths of the files unfolded, which stay unfolded when the diff is read again.
    open: HashSet<String>,
}

/// A read of a slice under way.
#[derive(Clone, Copy)]
struct Asked {
    ticket: u64,
    /// Whether it reads the first slice, which replaces those shown.
    first: bool,
}

/// One slice of the diff, as T3 sent it and split into files.
struct Slice {
    diff: pull_request::Diff,
    patch: patch::Patch,
    /// The places of its files in reading order, as `order::reading_order` says.
    order: Vec<usize>,
}

impl Code {
    /// The tab before anything was read, which wants the first slice.
    pub(super) fn new() -> Code {
        Code {
            slices: Vec::new(),
            files: Vec::new(),
            omitted: HashMap::new(),
            asked: None,
            failed: None,
            at: 0,
            again: true,
            due: false,
            open: HashSet::new(),
        }
    }

    /// Whether showing the tab at `now` should read the diff again: the first slice came
    /// `FRESH` or more ago, or a read of it failed, as the desktop reads a stale diff again when
    /// its tab mounts (`Atom.swr` with `revalidateOnMount` in
    /// packages/client-runtime/src/state/runtime.ts).
    pub(super) fn stale(&self, now: i64) -> bool {
        if self.asked.is_some() || self.again {
            return false;
        }
        let failed = self.failed.as_ref().is_some_and(|(_, first)| *first);
        failed || (!self.slices.is_empty() && now.saturating_sub(self.at) >= FRESH)
    }

    /// Notes that the diff may be out of date, so it is read again from its first slice.
    pub(super) fn again(&mut self) {
        self.again = true;
    }

    /// The cursor of the slice to read with `ticket`, or `None` inside for the first, when one
    /// is wanted and none is under way.
    pub(super) fn ask(&mut self, ticket: u64) -> Option<Option<String>> {
        if self.asked.is_some() {
            return None;
        }
        let cursor = if self.again {
            None
        } else if self.due && self.failed.is_none() {
            Some(self.next_cursor()?.to_string())
        } else {
            return None;
        };
        self.again = false;
        self.due = false;
        let first = cursor.is_none();
        self.asked = Some(Asked { ticket, first });
        Some(cursor)
    }

    /// Takes the answer to read `ticket`, which came at `now`. A first slice replaces the
    /// slices shown, and a later one follows them. A failure keeps them. Says whether what the
    /// tab shows changed, and if so whether it was a read of the first slice. An answer to
    /// another read, or one that came after the diff was wanted again, is dropped.
    pub(super) fn take(
        &mut self,
        ticket: u64,
        answer: Result<Box<pull_request::Diff>, String>,
        now: i64,
    ) -> Option<bool> {
        let asked = self.asked.take_if(|asked| asked.ticket == ticket)?;
        if self.again {
            return None;
        }
        match answer {
            Ok(diff) => {
                if asked.first {
                    self.slices.clear();
                    self.at = now;
                }
                let patch = patch::Patch::read(&diff.patch);
                let order = order::reading_order(&diff.patch, &patch);
                self.slices.push(Slice {
                    diff: *diff,
                    patch,
                    order,
                });
                self.failed = None;
                self.index();
            }
            Err(failure) => self.failed = Some((bounded(&failure, FAILURE), asked.first)),
        }
        Some(asked.first)
    }

    /// Works out the files of every slice and the counts of those the host withheld.
    fn index(&mut self) {
        self.files.clear();
        self.omitted.clear();
        for (at, slice) in self.slices.iter().enumerate() {
            self.files
                .extend(slice.order.iter().map(|&file| (at, file)));
            for omitted in &slice.diff.omitted {
                let counts = (omitted.additions, omitted.deletions);
                self.omitted.insert(omitted.path.clone(), counts);
            }
        }
    }

    /// The cursor of the slice after those read, while there is one.
    fn next_cursor(&self) -> Option<&str> {
        self.slices.last()?.diff.next_cursor.as_deref()
    }

    /// Whether the files read end in a row that asks for more: there is a next slice and its
    /// read didn't fail.
    pub(super) fn wants_more(&self) -> bool {
        self.next_cursor().is_some() && self.failed.is_none()
    }

    /// Notes that the end of the files came within a page of the view, so the next slice is
    /// read.
    pub(super) fn near_end(&mut self) {
        if self.wants_more() && self.asked.is_none() {
            self.due = true;
        }
    }

    /// Whether the next slice is to be read.
    pub(super) fn due(&self) -> bool {
        self.due && self.asked.is_none()
    }

    /// Whether `control` is one of the tab's and shows.
    pub(super) fn has(&self, control: Control) -> bool {
        match control {
            Control::File(at) => at < self.files.len(),
            Control::Retry => self.retries(),
            _ => false,
        }
    }

    /// Whether the end of the files offers Retry: the read of a later slice failed, and slices
    /// show above it with more after them.
    fn retries(&self) -> bool {
        matches!(self.failed, Some((_, false)))
            && !self.slices.is_empty()
            && self.next_cursor().is_some()
    }

    /// Does what `control` does: folds or unfolds a file, or reads again the slice whose read
    /// failed.
    pub(super) fn act(&mut self, control: Control) {
        match control {
            Control::File(at) => {
                let Some(path) = self.file(at).map(|(_, file)| file.path().to_string()) else {
                    return;
                };
                if !self.open.remove(&path) {
                    self.open.insert(path);
                }
            }
            Control::Retry => {
                if let Some((_, first)) = self.failed.take() {
                    self.again = first;
                    self.due = !first;
                }
            }
            _ => {}
        }
    }

    /// What Enter does with `control`, for the status line.
    pub(super) fn action(&self, control: Control) -> String {
        match control {
            Control::File(at) => {
                let open = self
                    .file(at)
                    .is_some_and(|(_, file)| self.open.contains(file.path()));
                if open { "collapse file" } else { "expand file" }.to_string()
            }
            _ => "retry".to_string(),
        }
    }

    /// The file at `at` among those shown, with the patch its lines are ranges of.
    fn file(&self, at: usize) -> Option<(&str, &patch::File)> {
        let &(slice, file) = self.files.get(at)?;
        let slice = &self.slices[slice];
        Some((&slice.diff.patch, &slice.patch.files[file]))
    }

    /// The tab's rows at `width`, with the rows of each file's header and of Retry. Until the
    /// first slice came, or when its read failed, it says so. Then the count of files, `+`
    /// while more are to be read, and a note when the host withheld part of the diff. Then each
    /// file, and at the end `Loading more files…`, or Retry once that failed.
    pub(super) fn lay_out(&self, width: usize, theme: &Theme) -> (Vec<Line<'static>>, Targets) {
        let muted = Style::new().fg(theme.muted);
        let mut lines = Vec::new();
        let mut targets = Vec::new();
        if width == 0 {
            return (lines, targets);
        }
        if self.slices.is_empty() {
            let text = match &self.failed {
                Some((failure, _)) => failure.as_str(),
                None => LOADING,
            };
            return (whole(text, width, muted), targets);
        }
        let count = self.files.len();
        let noun = if count == 1 { "file" } else { "files" };
        let more = if self.next_cursor().is_some() {
            "+"
        } else {
            ""
        };
        let count = format!("{} {noun}{more}", grouped(count as u64));
        lines.extend(whole(&count, width, muted));
        let unread = self
            .slices
            .iter()
            .any(|slice| slice.patch.files.is_empty() && !slice.diff.patch.trim().is_empty());
        if unread || self.slices.iter().any(|slice| slice.diff.truncated) {
            lines.extend(whole(WITHHELD, width, Style::new().fg(theme.warning_fg)));
        }
        // A read again of the first slice that failed leaves the files read before, which may
        // be out of date.
        if let Some((failure, true)) = &self.failed {
            lines.extend(whole(STALE, width, Style::new().fg(theme.warning_fg)));
            lines.extend(whole(failure, width, muted));
        }
        if self.slices.iter().any(|slice| slice.patch.files_cut) {
            let note = format!(
                "T3 sent more files in one slice than the {} t3term reads.",
                grouped(patch::MOST_FILES as u64)
            );
            lines.extend(whole(&note, width, muted));
        }
        if self.files.is_empty() && self.next_cursor().is_none() {
            lines.push(Line::default());
            let text = if unread { UNREAD } else { EMPTY };
            lines.extend(whole(text, width, muted));
            return (lines, targets);
        }
        for at in 0..self.files.len() {
            let (text, file) = self.file(at).expect("a file shown");
            let open = self.open.contains(file.path());
            lines.push(Line::default());
            let start = lines.len();
            let counts = match (file.additions, file.deletions) {
                (0, 0) => self.omitted.get(file.path()).copied().unwrap_or_default(),
                counts => counts,
            };
            lines.extend(header(file, open, counts, width, theme));
            targets.push((start..lines.len(), Control::File(at)));
            if open {
                let withheld = self.omitted.contains_key(file.path());
                lines.extend(body(text, file, withheld, width, theme));
            }
        }
        if self.next_cursor().is_some() && !self.failed.as_ref().is_some_and(|(_, first)| *first) {
            lines.push(Line::default());
            match &self.failed {
                Some((failure, _)) => {
                    lines.extend(whole(REST_FAILED, width, muted));
                    lines.extend(whole(failure, width, muted));
                    let start = lines.len();
                    lines.extend(whole("↻ Retry", width, Style::new().fg(theme.fg)));
                    targets.push((start..lines.len(), Control::Retry));
                }
                None => lines.extend(whole(LOADING_MORE, width, muted)),
            }
        }
        (lines, targets)
    }
}

/// A file's header: a chevron that says whether it is unfolded, a letter for how it changed,
/// its path, or both paths of a rename, then the lines it adds and removes unless both are 0,
/// as the desktop's `PullRequestDiffStat` leaves out. The path wraps under itself.
fn header(
    file: &patch::File,
    open: bool,
    (additions, deletions): (u64, u64),
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let chevron = if open { "⌄" } else { "›" };
    let (letter, color) = status(file.status, theme);
    let path = |path: Option<&str>| bounded(path.unwrap_or_default(), PATH);
    let name = match file.status {
        patch::Status::Renamed => {
            format!(
                "{} → {}",
                path(file.old_path.as_deref()),
                path(file.new_path.as_deref())
            )
        }
        _ => path(Some(file.path())),
    };
    let lead = [
        Span::styled(chevron, Style::new().fg(theme.muted)),
        Span::raw(" "),
        Span::styled(letter, Style::new().fg(color)),
        Span::raw(" "),
    ];
    let indent = 4;
    let mut stat = Vec::new();
    if additions != 0 || deletions != 0 {
        stat.push(Span::styled(
            format!("+{}", grouped(additions)),
            Style::new().fg(theme.success),
        ));
        stat.push(Span::raw(" "));
        stat.push(Span::styled(
            format!("-{}", grouped(deletions)),
            Style::new().fg(theme.error_fg),
        ));
    }
    if width <= indent {
        return vec![Line::from(Span::raw(clip(
            &format!("{chevron} {letter} {name}"),
            width,
        )))];
    }
    let fg = Style::new().fg(theme.fg);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (row, piece) in broken(&name, width - indent).into_iter().enumerate() {
        let mut spans = match row {
            0 => lead.to_vec(),
            _ => vec![Span::raw(" ".repeat(indent))],
        };
        spans.push(Span::styled(piece, fg));
        lines.push(Line::from(spans));
    }
    if !stat.is_empty() {
        let stat_width: usize = stat.iter().map(|span| span.width()).sum();
        let last = lines.last_mut().expect("a row for the path");
        if last.width() + 1 + stat_width <= width {
            last.spans.push(Span::raw(" "));
            last.spans.extend(stat);
        } else if indent + stat_width <= width {
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            spans.extend(stat);
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// The letter for how a change touched a file, as git's short status writes it, and its color.
fn status(status: patch::Status, theme: &Theme) -> (&'static str, Color) {
    match status {
        patch::Status::Added => ("A", theme.success),
        patch::Status::Deleted => ("D", theme.error_fg),
        patch::Status::Renamed => ("R", theme.info),
        patch::Status::Modified => ("M", theme.warning_fg),
    }
}

/// An unfolded file's rows: its hunks, each after a row that counts the unchanged lines before
/// it, and a note for each part t3term couldn't show. A file without hunks says why: it is
/// binary, the host withheld its changes, or the change touched no line, as a rename or a mode
/// change does.
fn body(
    text: &str,
    file: &patch::File,
    withheld: bool,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let mut lines = Vec::new();
    // A file past the bound or with a broken hunk header can have no hunks, and its notes
    // below say why.
    if file.hunks.is_empty() && !file.lines_cut && !file.unreadable {
        let note = if file.binary {
            "A binary file, whose changes have no lines to show."
        } else if withheld {
            "The host withheld this file's changes."
        } else {
            "No lines changed."
        };
        return whole(note, width, muted);
    }
    let last = file
        .hunks
        .iter()
        .map(|hunk| {
            let old = hunk.old_start + hunk.old_count.saturating_sub(1);
            old.max(hunk.new_start + hunk.new_count.saturating_sub(1))
        })
        .max()
        .unwrap_or_default();
    let digits = last.max(1).ilog10() as usize + 1;
    // The number, a space, the mark and a space, while the text keeps a few columns.
    let gutter = if width >= digits + 3 + 8 {
        Some(digits)
    } else {
        None
    };
    // Where the line after the last hunk is in the file before the change.
    let mut after = 1;
    for hunk in &file.hunks {
        // A hunk that removes nothing names the line before it, as git writes `-5,0`.
        let first = match hunk.old_count {
            0 => hunk.old_start + 1,
            _ => hunk.old_start,
        };
        let gap = first.saturating_sub(after);
        if gap > 0 {
            let noun = if gap == 1 { "line" } else { "lines" };
            let note = format!("⋯ {} unmodified {noun}", grouped(gap));
            lines.extend(whole(&note, width, muted));
        }
        after = first + hunk.old_count;
        for line in &hunk.lines {
            lines.extend(line_rows(text, line, gutter, width, theme));
        }
    }
    if file.lines_cut {
        let note = format!(
            "t3term reads at most {} lines of a slice, so the rest of this file doesn't show.",
            grouped(patch::MOST_LINES as u64)
        );
        lines.extend(whole(&note, width, muted));
    }
    if file.unreadable {
        lines.extend(whole(
            "t3term couldn't read some lines of this file's patch.",
            width,
            muted,
        ));
    }
    lines
}

/// A line of a hunk on as many rows as its text takes at `width`: its number in `digits`
/// columns where there is room for one, its mark and its text, in the color of what happened to
/// it. The rows after the first leave the number and the mark blank.
fn line_rows(
    text: &str,
    line: &patch::Line,
    gutter: Option<usize>,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let (mark, style) = match line.kind {
        patch::Kind::Added => ("+", Style::new().fg(theme.success)),
        patch::Kind::Removed => ("-", Style::new().fg(theme.error_fg)),
        patch::Kind::Context => (" ", Style::new().fg(theme.fg)),
        patch::Kind::NoNewline => (" ", muted),
    };
    let shown = match line.kind {
        patch::Kind::NoNewline => "No newline at end of file".to_string(),
        _ => shown(text.get(line.text.clone()).unwrap_or_default()),
    };
    let number = match line.kind {
        patch::Kind::Removed => line.old,
        patch::Kind::NoNewline => None,
        _ => line.new.or(line.old),
    };
    // A column of its own leaves none for the text.
    let lead = match gutter {
        Some(digits) => digits + 3,
        None if width > 1 => 1,
        None => 0,
    };
    let room = width.saturating_sub(lead).max(1);
    let mut rows = Vec::new();
    for (at, piece) in broken(&shown, room).into_iter().enumerate() {
        let mut spans = Vec::with_capacity(3);
        match (gutter, at) {
            (Some(digits), 0) => {
                let number = number.map(|n| n.to_string()).unwrap_or_default();
                spans.push(Span::styled(format!("{number:>digits$} "), muted));
                spans.push(Span::styled(format!("{mark} "), style));
            }
            (None, 0) if lead > 0 => spans.push(Span::styled(mark, style)),
            _ => spans.push(Span::raw(" ".repeat(lead))),
        }
        spans.push(Span::styled(clip(&piece, room), style));
        rows.push(Line::from(spans));
    }
    rows
}

/// A line of the diff as it shows: its tabs set every `TAB` columns, without control
/// characters, and cut with `…` past `LINE` characters.
fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(LINE * 4));
    let mut column = 0;
    for (count, c) in text.chars().enumerate() {
        if count == LINE {
            out.push('…');
            break;
        }
        match c {
            '\t' => {
                let spaces = TAB - column % TAB;
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            c if c.is_control() => {}
            c => {
                out.push(c);
                column += c.width().unwrap_or(0);
            }
        }
    }
    out
}

/// `text` broken into rows of at most `width` columns, between any two characters, so its
/// spaces all stay. An empty text takes one row.
fn broken(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut used = 0;
    for piece in clusters(text) {
        let columns = unicode_width::UnicodeWidthStr::width(piece);
        if used > 0 && used + columns > width {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("a row").push_str(piece);
        used += columns;
    }
    rows
}

mod order;

#[cfg(test)]
mod tests;
